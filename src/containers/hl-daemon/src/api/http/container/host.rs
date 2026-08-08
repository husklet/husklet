use super::*;

#[derive(Debug, Default)]
pub(super) struct HostSettings {
    pub(super) mounts: Vec<Mount>,
    pub(super) anonymous: Vec<String>,
    pub(super) resources: Resources,
    pub(super) isolation: Isolation,
    restart: hl_container::RestartPolicy,
    pub(super) removal: hl_container::RemovalPolicy,
    pub(super) ports: std::collections::BTreeSet<hl_container::Port>,
    pub(super) publish: Vec<hl_container::Publication>,
    pub(super) network_mode: hl_container::NetworkMode,
    pub(super) hosts: BTreeMap<String, std::net::IpAddr>,
    pub(super) resolver: hl_container::Resolver,
}

impl HostSettings {
    pub(super) async fn parse(
        value: Option<&HostConfig>,
        exposed: &crate::api::ExposedPorts,
        declared: BTreeMap<String, serde_json::Value>,
        network_isolated: bool,
        network_mode: hl_container::NetworkMode,
        containers: &hl_container::Containers,
    ) -> ApiResult<Self> {
        let fallback = HostConfig::default();
        let value = value.unwrap_or(&fallback);
        value.validate_unsupported()?;
        if !value.links.is_empty() {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "HostConfig.Links is not implemented",
            ));
        }

        let resources = Self::resources(value)?;
        let hosts = Self::hosts(value)?;
        let resolver =
            hl_container::Resolver::new(value.dns.clone(), value.dns_search.clone(), value.dns_options.clone())
                .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        let isolation = Self::isolation(value, network_isolated, network_mode);
        let restart = value
            .restart_policy
            .policy()
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;
        let bindings = if network_mode == hl_container::NetworkMode::Host {
            crate::api::PortBindings::default()
        } else {
            value.port_bindings.clone()
        };
        let (ports, publish) = bindings
            .ports(exposed, containers)
            .await
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;

        let mut mounts =
            Vec::with_capacity(value.binds.len() + value.mounts.len() + value.tmpfs.len() + declared.len());
        let mut anonymous = Vec::new();
        for bind in &value.binds {
            let (mount, owned) = LegacyBind::from(bind.as_str()).mount(containers).await?;
            mounts.push(mount);
            if let Some(name) = owned {
                anonymous.push(name);
            }
        }
        for mount in &value.mounts {
            mount.validate_unsupported()?;
            let (mount, owned) = mount.mount(containers).await?;
            mounts.push(mount);
            if let Some(name) = owned {
                anonymous.push(name);
            }
        }
        for (target, options) in &value.tmpfs {
            let (mount, owned) = Self::tmpfs(target, options, containers).await?;
            mounts.push(mount);
            anonymous.push(owned);
        }
        Self::declared(declared, containers, &mut mounts, &mut anonymous).await?;

        Ok(Self {
            mounts,
            anonymous,
            resources,
            isolation,
            restart,
            removal: if value.auto_remove {
                hl_container::RemovalPolicy::Automatic
            } else {
                hl_container::RemovalPolicy::Retain
            },
            ports,
            publish,
            network_mode,
            hosts,
            resolver,
        })
    }

    fn hosts(value: &HostConfig) -> ApiResult<BTreeMap<String, std::net::IpAddr>> {
        value
            .extra_hosts
            .iter()
            .map(|entry| {
                let delimiter = entry.find([':', '=']).ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        format!("invalid ExtraHosts entry {entry:?}; expected host:ip"),
                    )
                })?;
                let (name, address) = entry.split_at(delimiter);
                let address = address[1..].parse().map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        format!("invalid ExtraHosts address in {entry:?}"),
                    )
                })?;
                if name.is_empty() {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "ExtraHosts name must not be empty",
                    ));
                }
                Ok((name.to_owned(), address))
            })
            .collect()
    }

    pub(super) async fn tmpfs(
        target: &str,
        options: &str,
        containers: &hl_container::Containers,
    ) -> ApiResult<(Mount, String)> {
        if !matches!(options, "" | "rw") {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("tmpfs options {options:?} are not implemented"),
            ));
        }
        let target = std::path::PathBuf::from(Target::try_from(target)?);
        let volume = containers
            .volumes()
            .create_anonymous(std::iter::empty::<(String, String)>())
            .await
            .map_err(ApiError::container)?;
        let name = volume.name.clone();
        Ok((Mount::tmpfs(&volume, target), name))
    }

    fn resources(value: &HostConfig) -> ApiResult<Resources> {
        let memory_bytes = u64::try_from(value.memory)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "HostConfig.Memory must be nonnegative"))?;
        let process_count = match value.pids_limit.unwrap_or_default() {
            -1 | 0 => 0,
            limit if limit > 0 => u32::try_from(limit)
                .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "HostConfig.PidsLimit exceeds u32"))?,
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "HostConfig.PidsLimit must be -1, 0, or positive",
                ));
            }
        };
        let nano_cpus = u64::try_from(value.nano_cpus)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "HostConfig.NanoCpus must be nonnegative"))?;
        let cpu_count = if nano_cpus == 0 {
            0
        } else {
            let count = nano_cpus.div_ceil(1_000_000_000);
            u32::try_from(count)
                .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "HostConfig.NanoCpus exceeds u32 CPUs"))?
        };

        Ok(Resources {
            memory_bytes,
            process_count,
            cpu_count,
            // Docker's `HostConfig.Ulimits` is not parsed yet, so an API launch keeps the engine defaults.
            limits: Vec::new(),
        })
    }

    fn isolation(value: &HostConfig, network_isolated: bool, network_mode: hl_container::NetworkMode) -> Isolation {
        Isolation {
            sandbox: if network_mode == hl_container::NetworkMode::Host {
                hl_container::Sandbox::Disabled
            } else {
                hl_container::Sandbox::default()
            },
            read_only_root: value.readonly_rootfs,
            network_isolated,
            // Docker's `--security-opt seccomp=` is not parsed yet, so every API launch keeps the container baseline.
            seccomp_baseline: hl_container::SeccompBaseline::default(),
        }
    }

    async fn declared(
        declared: BTreeMap<String, serde_json::Value>,
        containers: &hl_container::Containers,
        mounts: &mut Vec<Mount>,
        anonymous: &mut Vec<String>,
    ) -> ApiResult<()> {
        for (target, options) in declared {
            if options.as_object().is_none_or(|options| !options.is_empty()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("volume declaration for {target:?} must be an empty object"),
                ));
            }
            if mounts.iter().any(|mount| mount.target == std::path::Path::new(&target)) {
                continue;
            }
            let volume = containers
                .volumes()
                .create_anonymous(std::iter::empty::<(String, String)>())
                .await
                .map_err(ApiError::container)?;
            anonymous.push(volume.name.clone());
            mounts.push(
                Mount::anonymous(
                    &volume,
                    Target::try_from(target.as_str())?,
                    hl_container::Access::ReadWrite,
                )
                .populate(),
            );
        }
        Ok(())
    }

    pub(super) fn apply(self, mut spec: ContainerSpec) -> ContainerSpec {
        for mount in self.mounts {
            spec = spec.mount(mount);
        }
        for port in self.ports {
            spec = spec.expose(port);
        }
        for publish in self.publish {
            spec = spec.publish(publish);
        }
        for (name, address) in self.hosts {
            spec = spec.host(name, address);
        }
        spec = spec.resolver(self.resolver);
        spec.resources(self.resources)
            .isolation(self.isolation)
            .network_mode(self.network_mode)
            .restart(self.restart)
            .removal(self.removal)
    }
}
