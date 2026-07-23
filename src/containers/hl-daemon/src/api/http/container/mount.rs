use super::*;

pub(super) struct LegacyBind<'a>(&'a str);

impl<'a> From<&'a str> for LegacyBind<'a> {
    fn from(value: &'a str) -> Self {
        Self(value)
    }
}

impl LegacyBind<'_> {
    pub(super) async fn mount(
        &self,
        containers: &hl_container::Containers,
    ) -> ApiResult<(Mount, Option<String>)> {
        let value = self.0;
        let fields = value.split(':').collect::<Vec<_>>();
        let (source, target, options) = match fields.as_slice() {
            [target] => ("", *target, ""),
            [source, target] => (*source, *target, ""),
            [source, target, options] => (*source, *target, *options),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid bind mount {value:?}; expected SOURCE:TARGET[:OPTIONS]"),
                ));
            }
        };
        let mut read_only = false;
        for option in options.split(',').filter(|option| !option.is_empty()) {
            match option {
                "ro" => read_only = true,
                "rw" => read_only = false,
                option => {
                    return Err(ApiError::new(
                        StatusCode::NOT_IMPLEMENTED,
                        format!("bind option {option:?} is not implemented"),
                    ));
                }
            }
        }
        requested_mount(source, target, read_only, BTreeMap::new(), true, containers).await
    }
}

pub(super) async fn requested_mount(
    source: &str,
    target: &str,
    read_only: bool,
    labels: BTreeMap<String, String>,
    populate: bool,
    containers: &hl_container::Containers,
) -> ApiResult<(Mount, Option<String>)> {
    let target = std::path::PathBuf::from(Target::try_from(target)?);
    let access = if read_only {
        hl_container::Access::ReadOnly
    } else {
        hl_container::Access::ReadWrite
    };
    if source.is_empty() {
        let volume = containers
            .volumes()
            .create_anonymous(labels)
            .await
            .map_err(ApiError::container)?;
        let name = volume.name.clone();
        let mount = Mount::anonymous(&volume, target, access);
        let mount = if populate { mount.populate() } else { mount };
        return Ok((mount, Some(name)));
    }
    if !std::path::Path::new(source).is_absolute() {
        let volumes = containers.volumes();
        let volume = match volumes.inspect(source).await {
            Ok(volume) => volume,
            Err(hl_container::Error::VolumeNotFound(_)) => {
                let mut spec = hl_container::VolumeSpec::new(source);
                spec.labels = labels;
                volumes.create(spec).await.map_err(ApiError::container)?
            }
            Err(error) => return Err(ApiError::container(error)),
        };
        let mount = Mount::volume(volume.name, target, access);
        let mount = if populate { mount.populate() } else { mount };
        return Ok((mount, None));
    }
    Ok((
        if read_only {
            Mount::read_only(source, target)
        } else {
            Mount::read_write(source, target)
        },
        None,
    ))
}

impl crate::api::DockerMount {
    pub(super) fn validate_unsupported(&self) -> ApiResult<()> {
        let fields = crate::api::CompatibilityFields::from(&self.unsupported);
        let Some(name) = fields.first_meaningful() else {
            return Ok(());
        };
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("HostConfig.Mounts entry field is not implemented: {name}"),
        ))
    }

    pub(super) async fn mount(
        &self,
        containers: &hl_container::Containers,
    ) -> ApiResult<(Mount, Option<String>)> {
        match self.kind.as_str() {
            "bind" => self.bind(containers).await,
            "volume" => self.volume(containers).await,
            "tmpfs" => {
                if !self.source.is_empty()
                    || self.read_only
                    || self.bind_options.is_some()
                    || self.volume_options.is_some()
                {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "tmpfs mount only supports an absolute Target",
                    ));
                }
                let (mount, name) = HostSettings::tmpfs(&self.target, "", containers).await?;
                Ok((mount, Some(name)))
            }
            kind => Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("mount type {kind:?} is not implemented"),
            )),
        }
    }

    async fn bind(
        &self,
        containers: &hl_container::Containers,
    ) -> ApiResult<(Mount, Option<String>)> {
        let mut propagation = hl_container::BindPropagation::RecursivePrivate;
        if let Some(options) = &self.bind_options {
            propagation = match options.propagation.as_str() {
                "" | "rprivate" => hl_container::BindPropagation::RecursivePrivate,
                "private" => hl_container::BindPropagation::Private,
                "shared" | "rshared" | "slave" | "rslave" | "unbindable" | "runbindable" => {
                    return Err(ApiError::new(
                        StatusCode::NOT_IMPLEMENTED,
                        format!(
                            "bind propagation {:?} requires host mount propagation support",
                            options.propagation
                        ),
                    ))
                }
                value => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        format!("invalid bind propagation {value:?}"),
                    ))
                }
            };
            if options.non_recursive {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "non-recursive bind mounts require nested host-mount boundary support",
                ));
            }
            if options.create_mountpoint {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "bind CreateMountpoint is not implemented",
                ));
            }
            if options.read_only.read_only_non_recursive
                && options.read_only.read_only_force_recursive
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "recursive and non-recursive read-only options conflict",
                ));
            }
            if (options.read_only.read_only_non_recursive
                || options.read_only.read_only_force_recursive)
                && !self.read_only
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bind read-only recursion options require ReadOnly=true",
                ));
            }
            if options.read_only.read_only_non_recursive {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "non-recursive read-only requires nested host-mount boundary support",
                ));
            }
        }
        if self.volume_options.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "VolumeOptions are invalid for a bind mount",
            ));
        }
        if self.source.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bind mount source must not be empty",
            ));
        }
        if !std::path::Path::new(&self.source).is_absolute() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bind mount source must be absolute",
            ));
        }
        let (mount, anonymous) = requested_mount(
            &self.source,
            &self.target,
            self.read_only,
            BTreeMap::new(),
            false,
            containers,
        )
        .await?;
        Ok((mount.propagation(propagation), anonymous))
    }

    async fn volume(
        &self,
        containers: &hl_container::Containers,
    ) -> ApiResult<(Mount, Option<String>)> {
        if self.bind_options.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "BindOptions are invalid for a volume mount",
            ));
        }
        let mut labels = BTreeMap::new();
        let mut populate = true;
        let mut subpath = None;
        let mut driver_options = None;
        if let Some(options) = &self.volume_options {
            labels = options.labels.clone().unwrap_or_default();
            populate = !options.no_copy;
            subpath = options.subpath.as_deref();
            if let Some(driver) = &options.driver_config {
                if !driver.name.is_empty() && driver.name != "local" {
                    return Err(ApiError::new(
                        StatusCode::NOT_IMPLEMENTED,
                        format!("volume driver {:?} is not implemented", driver.name),
                    ));
                }
                driver_options = Some(driver.options.clone());
            }
        }
        if let Some(options) = driver_options.filter(|options| !options.is_empty()) {
            if self.source.is_empty() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "local bind volume options require an explicit volume source",
                ));
            }
            let backing = super::super::volume::LocalOptions::parse(&options)?
                .expect("non-empty local options must produce a backing source");
            let mut spec =
                hl_container::VolumeSpec::new(&self.source).bind(backing.device, backing.read_only);
            spec.labels = labels;
            spec.options = options;
            let volume = containers
                .volumes()
                .create(spec)
                .await
                .map_err(ApiError::container)?;
            let access = if self.read_only {
                hl_container::Access::ReadOnly
            } else {
                hl_container::Access::ReadWrite
            };
            let mut selected =
                Mount::volume(volume.name, Target::try_from(self.target.as_str())?, access);
            if populate {
                selected = selected.populate();
            }
            if let Some(value) = subpath {
                selected = selected.subpath(value).map_err(ApiError::container)?;
            }
            return Ok((selected, None));
        }
        let (mount, anonymous) = requested_mount(
            &self.source,
            &self.target,
            self.read_only,
            labels,
            populate,
            containers,
        )
        .await?;
        let mount = if let Some(value) = subpath {
            mount.subpath(value).map_err(ApiError::container)?
        } else {
            mount
        };
        Ok((mount, anonymous))
    }
}

pub(super) struct Target(std::path::PathBuf);

impl From<Target> for std::path::PathBuf {
    fn from(target: Target) -> Self {
        target.0
    }
}

impl TryFrom<&str> for Target {
    type Error = ApiError;

    fn try_from(target: &str) -> Result<Self, Self::Error> {
        let path = std::path::Path::new(target);
        if !path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("mount target {target:?} must be absolute and normalized"),
            ));
        }
        Ok(Self(target.into()))
    }
}
