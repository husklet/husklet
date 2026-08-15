use super::DirectoryImage;
use crate::checkpoint::CheckpointError;
#[cfg(not(unix))]
use std::path::Path;

impl DirectoryImage {
    #[cfg(not(unix))]
    pub(super) fn collect(
        root: &Path,
        directory: &Path,
        exclude_generation_metadata: bool,
        objects: &mut Vec<String>,
    ) -> Result<(), CheckpointError> {
        Self::collect_until(root, directory, exclude_generation_metadata, objects, None)
    }

    #[cfg(not(unix))]
    pub(super) fn collect_until(
        root: &Path,
        directory: &Path,
        exclude_generation_metadata: bool,
        objects: &mut Vec<String>,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), CheckpointError> {
        Self::check_deadline(deadline)?;
        for entry in std::fs::read_dir(directory)
            .map_err(|error| CheckpointError::new(format!("list checkpoint objects: {error}")))?
        {
            Self::check_deadline(deadline)?;
            let entry = entry.map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
            if exclude_generation_metadata
                && directory == root
                && (entry.file_name() == "current"
                    || entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("generation-")))
            {
                continue;
            }
            Self::check_deadline(deadline)?;
            let kind = entry
                .file_type()
                .map_err(|error| CheckpointError::new(error.to_string()))?;
            if kind.is_symlink() {
                return Err(CheckpointError::new("checkpoint image contains a symbolic link"));
            }
            if kind.is_dir() {
                Self::collect_until(root, &entry.path(), exclude_generation_metadata, objects, deadline)?;
            } else {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| CheckpointError::new(error.to_string()))?
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                objects.push(relative);
            }
        }
        Self::check_deadline(deadline)?;
        Ok(())
    }
    #[cfg(unix)]
    pub(super) fn collect_held(
        directory: std::os::fd::OwnedFd,
        prefix: &str,
        exclude_generation_metadata: bool,
        objects: &mut Vec<String>,
    ) -> Result<(), CheckpointError> {
        Self::collect_held_until(directory, prefix, exclude_generation_metadata, objects, None)
    }

    #[cfg(unix)]
    pub(super) fn collect_held_until(
        directory: std::os::fd::OwnedFd,
        prefix: &str,
        exclude_generation_metadata: bool,
        objects: &mut Vec<String>,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};

        Self::check_deadline(deadline)?;
        let mut directory = nix::dir::Dir::from_fd(directory)
            .map_err(|error| CheckpointError::new(format!("list checkpoint objects: {error}")))?;
        let names = directory
            .iter()
            .map(|entry| {
                Self::check_deadline(deadline)?;
                entry
                    .map(|entry| entry.file_name().to_owned())
                    .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for name in names {
            Self::check_deadline(deadline)?;
            let name = name.as_c_str();
            if name == c"." || name == c".." {
                continue;
            }
            let text = name
                .to_str()
                .map_err(|_| CheckpointError::new("checkpoint object name is not UTF-8"))?;
            if exclude_generation_metadata
                && prefix.is_empty()
                && (text == "current" || text.starts_with("generation-"))
            {
                continue;
            }
            let descriptor = openat(
                &directory,
                name,
                OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
                Mode::empty(),
            )
            .map_err(|error| CheckpointError::new(format!("open checkpoint object: {error}")))?;
            let kind = SFlag::from_bits_truncate(
                fstat(&descriptor)
                    .map_err(|error| CheckpointError::new(format!("inspect checkpoint object: {error}")))?
                    .st_mode,
            );
            let relative = if prefix.is_empty() {
                text.to_owned()
            } else {
                format!("{prefix}/{text}")
            };
            if kind.contains(SFlag::S_IFDIR) {
                Self::collect_held_until(descriptor, &relative, exclude_generation_metadata, objects, deadline)?;
            } else if kind.contains(SFlag::S_IFREG) {
                objects.push(relative);
            } else {
                return Err(CheckpointError::new("checkpoint image contains a non-regular object"));
            }
        }
        Self::check_deadline(deadline)?;
        Ok(())
    }
}
