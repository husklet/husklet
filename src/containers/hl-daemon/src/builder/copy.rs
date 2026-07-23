use super::context::{Context, Pattern};
use super::remote::RemoteSources;
use super::{Build, BuildError, Builder};
use hl_container::Process;
use hl_images::build::{Account, CopySource, OwnershipSpec, Step};
use hl_images::snapshot::{Ownership, Ownerships};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

impl Builder {
    pub(super) fn copy_step(
        &self,
        context: CopyContext<'_>,
        step: &Step,
        ownerships: &mut Ownerships,
    ) -> Result<(), BuildError> {
        let Step::Copy {
            sources,
            target,
            directory,
            from,
            unpack,
            mode,
            ownership,
            excludes,
            parents,
            checksum: _,
        } = step
        else {
            return Err(hl_images::Error::MalformedOci("expected COPY step".into()).into());
        };
        let resolved_owner = ownership
            .as_ref()
            .map(|ownership| Accounts::new(context.root).resolve(ownership))
            .transpose()?;
        let mut apply = |source, selected: &[String], unpack| {
            Copy {
                source,
                sources: selected,
                target,
                directory: directory
                    .as_deref()
                    .unwrap_or(&context.inherited.working_directory),
                destination: context.root,
                unpack,
                mode: *mode,
                owner: resolved_owner,
                excludes,
                parents: *parents,
            }
            .apply(ownerships)
        };
        let local = sources
            .iter()
            .filter(|source| matches!(source, hl_images::build::Source::Local(_)))
            .map(|source| source.as_str().to_owned())
            .collect::<Vec<_>>();
        for source in sources.iter().filter(|source| source.is_remote()) {
            let remote = context.remotes.get(source.as_str())?;
            let selected = [remote.name().to_owned()];
            apply(remote.root(), &selected, false)?;
        }
        if local.is_empty() {
            return Ok(());
        }
        match from {
            None => apply(context.context.root(), &local, *unpack),
            Some(CopySource::Stage(index)) => {
                let source = context.built.get(*index).ok_or_else(|| {
                    hl_images::Error::MalformedOci("COPY depends on an unavailable stage".into())
                })?;
                apply(source.root.path(), &local, *unpack)
            }
            Some(CopySource::Image(reference)) => {
                let images = self.containers.images()?;
                let image = images.resolve(reference)?.ok_or_else(|| {
                    hl_images::Error::InvalidMetadata(format!(
                        "COPY source image {reference} is not local"
                    ))
                })?;
                let unpacked = images.unpack(&image, &self.platform)?;
                let owned = images.rootfs(&unpacked)?;
                let view = images.roots().open(&owned)?;
                let result = apply(view.path(), &local, *unpack);
                images.roots().release(&owned)?;
                result
            }
        }
    }
}

struct Accounts<'a> {
    root: &'a Path,
}

impl<'a> Accounts<'a> {
    fn new(root: &'a Path) -> Self {
        Self { root }
    }

    fn resolve(&self, value: &OwnershipSpec) -> Result<Ownership, BuildError> {
        if let (Account::Id(uid), None) = (&value.user, &value.group) {
            return Ok(Ownership {
                uid: *uid,
                gid: *uid,
            });
        }
        let mut identity = value.user.to_string();
        if let Some(group) = &value.group {
            identity.push(':');
            identity.push_str(&group.to_string());
        }
        let (uid, gid) = Process::resolve_user(&identity, self.root)?;
        Ok(Ownership {
            uid: u32::try_from(uid).map_err(|_| {
                hl_images::Error::MalformedOci(format!("COPY --chown uid {uid} is invalid"))
            })?,
            gid: u32::try_from(gid).map_err(|_| {
                hl_images::Error::MalformedOci(format!("COPY --chown gid {gid} is invalid"))
            })?,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct CopyContext<'a> {
    pub(super) context: &'a Context<'a>,
    pub(super) built: &'a [Build],
    pub(super) root: &'a Path,
    pub(super) inherited: &'a hl_images::RuntimeConfig,
    pub(super) remotes: &'a RemoteSources,
}

pub(super) fn copy_root(
    source: &Path,
    ownerships: &Ownerships,
    target: &Path,
) -> Result<Ownerships, BuildError> {
    let mut bytes = Vec::new();
    ownerships.archive(source, &mut bytes)?;
    let mut copied = Ownerships::memory();
    hl_images::layer::Layer::new(&bytes[..]).apply_with_ownership(target, &mut copied)?;
    Ok(copied)
}

pub(super) struct Copy<'a> {
    pub(super) source: &'a Path,
    pub(super) sources: &'a [String],
    pub(super) target: &'a str,
    pub(super) directory: &'a str,
    pub(super) destination: &'a Path,
    pub(super) unpack: bool,
    pub(super) mode: Option<u32>,
    pub(super) owner: Option<Ownership>,
    pub(super) excludes: &'a [String],
    pub(super) parents: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Archive {
    Gzip,
    Tar,
}

impl Archive {
    pub(super) fn detect(path: &Path) -> Result<Option<Self>, BuildError> {
        let mut file = std::fs::File::open(path)?;
        let mut magic = [0_u8; 2];
        if file.read(&mut magic)? == magic.len() && magic == [0x1f, 0x8b] {
            return Ok(Some(Self::Gzip));
        }
        Ok(Self::is_tar(path)?.then_some(Self::Tar))
    }

    fn is_tar(path: &Path) -> Result<bool, BuildError> {
        let file = std::fs::File::open(path)?;
        let length = file.metadata()?.len();
        let mut archive = tar::Archive::new(file);
        let Ok(entries) = archive.entries() else {
            return Ok(false);
        };
        let mut count = 0_usize;
        for entry in entries {
            if entry.is_err() {
                return Ok(false);
            }
            count += 1;
        }
        if count != 0 {
            return Ok(true);
        }
        if length < 1_024 {
            return Ok(false);
        }
        let mut end = [1_u8; 1_024];
        let mut file = std::fs::File::open(path)?;
        file.read_exact(&mut end)?;
        Ok(end.iter().all(|byte| *byte == 0))
    }

    fn unpack(
        self,
        source: &Path,
        destination: &Path,
        ownerships: &mut Ownerships,
    ) -> Result<(), BuildError> {
        match self {
            Self::Gzip => hl_images::layer::Layer::new(flate2::read::GzDecoder::new(
                std::fs::File::open(source)?,
            ))
            .apply_with_ownership(destination, ownerships)?,
            Self::Tar => hl_images::layer::Layer::new(std::fs::File::open(source)?)
                .apply_with_ownership(destination, ownerships)?,
        };
        Ok(())
    }
}

impl Copy<'_> {
    pub(super) fn apply(&self, ownerships: &mut Ownerships) -> Result<(), BuildError> {
        let mut copied = 0_usize;
        let directory_target = self.target.ends_with('/');
        let target = if self.target.starts_with('/') {
            PathBuf::from(self.target.trim_start_matches('/'))
        } else {
            PathBuf::from(self.directory.trim_start_matches('/')).join(self.target)
        }
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<PathBuf>();
        for source in self.sources {
            let source_path = Context::new(self.source).source(source)?;
            if source_path.is_file()
                && self
                    .excludes
                    .iter()
                    .any(|pattern| Pattern::new(pattern).matches(source.trim_start_matches('/')))
            {
                continue;
            }
            let archive = source_path
                .is_file()
                .then(|| Archive::detect(&source_path))
                .transpose()?
                .flatten();
            if let (true, Some(archive)) = (self.unpack, archive) {
                let output = self.destination.join(&target);
                std::fs::create_dir_all(&output)?;
                let mut imported = Ownerships::memory();
                archive.unpack(&source_path, &output, &mut imported)?;
                ownerships.merge(&target, &imported)?;
                self.apply_metadata(&output)?;
                if let Some(owner) = self.owner {
                    ownerships.set_recursive(self.destination, &target, owner)?;
                }
                copied = copied.saturating_add(1);
                continue;
            }
            let mut path = target.clone();
            if self.parents {
                path.push(Self::parents_path(source)?);
            } else if !source_path.is_dir() && (self.sources.len() > 1 || directory_target) {
                path.push(
                    source_path
                        .file_name()
                        .ok_or_else(|| BuildError::Copy(source.clone()))?,
                );
            }
            let mut bytes = Vec::new();
            {
                let mut archive = tar::Builder::new(&mut bytes);
                if source_path.is_dir() {
                    self.append(&mut archive, &source_path, &path)?;
                } else {
                    archive.append_path_with_name(&source_path, &path)?;
                }
                archive.finish()?;
            }
            hl_images::layer::Layer::new(&bytes[..]).apply(self.destination)?;
            let output = self.destination.join(&path);
            self.apply_metadata(&output)?;
            if let Some(owner) = self.owner {
                ownerships.set_recursive(self.destination, &path, owner)?;
            } else {
                ownerships.set_recursive(self.destination, &path, Ownership { uid: 0, gid: 0 })?;
            }
            copied = copied.saturating_add(1);
        }
        if copied == 0 {
            return Err(BuildError::Copy(
                "all COPY/ADD sources were excluded".into(),
            ));
        }
        Ok(())
    }

    fn apply_metadata(&self, path: &Path) -> Result<(), BuildError> {
        let info = std::fs::symlink_metadata(path)?;
        if info.file_type().is_symlink() {
            return Ok(());
        }
        if let Some(mode) = self.mode {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        if info.is_dir() {
            for entry in std::fs::read_dir(path)? {
                self.apply_metadata(&entry?.path())?;
            }
        }
        Ok(())
    }

    fn parents_path(value: &str) -> Result<PathBuf, BuildError> {
        let value = value
            .split_once("/./")
            .map_or(value, |(_, relative)| relative)
            .trim_start_matches('/')
            .trim_start_matches("./");
        let path = Path::new(value);
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(BuildError::Copy(format!(
                "invalid --parents source {value:?}"
            )));
        }
        Ok(path.into())
    }

    fn append<W: Write>(
        &self,
        archive: &mut tar::Builder<W>,
        source: &Path,
        target: &Path,
    ) -> Result<(), BuildError> {
        archive.append_dir(target, source)?;
        self.append_children(archive, source, source, target)
    }

    fn append_children<W: Write>(
        &self,
        archive: &mut tar::Builder<W>,
        root: &Path,
        directory: &Path,
        target: &Path,
    ) -> Result<(), BuildError> {
        let mut entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let source = entry.path();
            let relative = source.strip_prefix(root).expect("COPY descendant");
            let display = relative.to_string_lossy();
            if self
                .excludes
                .iter()
                .any(|pattern| Pattern::new(pattern).matches(&display))
            {
                continue;
            }
            let destination = target.join(relative);
            if entry.file_type()?.is_dir() {
                archive.append_dir(&destination, &source)?;
                self.append_children(archive, root, &source, target)?;
            } else {
                archive.append_path_with_name(&source, destination)?;
            }
        }
        Ok(())
    }
}
