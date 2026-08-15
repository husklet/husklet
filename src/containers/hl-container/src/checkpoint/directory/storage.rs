use super::{DirectoryImage, GENERATION};
use crate::checkpoint::CheckpointError;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::Ordering;

pub(super) enum PublicationOutcome {
    Durable,
    PublishedNotDurable(CheckpointError),
}

impl DirectoryImage {
    fn check_deadline(deadline: Option<std::time::Instant>) -> Result<(), CheckpointError> {
        deadline
            .is_none_or(|deadline| std::time::Instant::now() < deadline)
            .then_some(())
            .ok_or_else(CheckpointError::deadline)
    }

    #[cfg(unix)]
    fn create_directory(directory: &std::os::fd::OwnedFd, component: &std::ffi::OsStr) -> Result<(), CheckpointError> {
        use nix::sys::stat::{Mode, mkdirat};

        match mkdirat(directory, component, Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => Ok(()),
            Err(error) => Err(CheckpointError::new(format!(
                "create checkpoint object directory: {error}"
            ))),
        }
    }

    #[cfg(unix)]
    fn open_child_directory(
        directory: &std::os::fd::OwnedFd,
        component: &std::ffi::OsStr,
        create: bool,
    ) -> Result<std::os::fd::OwnedFd, CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;

        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
        match openat(directory, component, flags, Mode::empty()) {
            Ok(child) => Ok(child),
            Err(nix::errno::Errno::ENOENT) if create => {
                Self::create_directory(directory, component)?;
                openat(directory, component, flags, Mode::empty())
                    .map_err(|error| CheckpointError::new(format!("open checkpoint object directory: {error}")))
            }
            Err(error) => Err(CheckpointError::new(format!(
                "open checkpoint object directory: {error}"
            ))),
        }
    }

    #[cfg(unix)]
    fn parent(
        root: &std::os::fd::OwnedFd,
        name: &str,
        create: bool,
    ) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;
        use std::os::fd::OwnedFd;

        let path = Self::path(Path::new(""), name)?;
        let mut components = path
            .components()
            .map(|component| component.as_os_str().to_owned())
            .collect::<Vec<_>>();
        let leaf = components
            .pop()
            .ok_or_else(|| CheckpointError::new("checkpoint object name is empty"))?;
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
        let mut directory: OwnedFd = openat(root, ".", flags, Mode::empty())
            .map_err(|error| CheckpointError::new(format!("hold checkpoint generation: {error}")))?;
        for component in components {
            directory = Self::open_child_directory(&directory, component.as_os_str(), create)?;
        }
        Ok((directory, leaf))
    }

    #[cfg(unix)]
    pub(super) fn read(root: &std::os::fd::OwnedFd, name: &str) -> Result<Vec<u8>, CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};
        use std::io::Read as _;

        let (directory, leaf) = Self::parent(root, name, false)?;
        let descriptor = openat(
            &directory,
            leaf.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        let kind = SFlag::from_bits_truncate(
            fstat(&descriptor)
                .map_err(|error| CheckpointError::new(format!("inspect checkpoint object: {error}")))?
                .st_mode,
        );
        if !kind.contains(SFlag::S_IFREG) {
            return Err(CheckpointError::new("checkpoint object is not a regular file"));
        }
        let mut bytes = Vec::new();
        std::fs::File::from(descriptor)
            .read_to_end(&mut bytes)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        Ok(bytes)
    }

    #[cfg(unix)]
    pub(super) fn read_optional(root: &std::os::fd::OwnedFd, name: &str) -> Result<Option<Vec<u8>>, CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};
        use std::io::Read as _;

        let descriptor = match openat(
            root,
            name,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(nix::errno::Errno::ENOENT) => return Ok(None),
            Err(error) => {
                return Err(CheckpointError::new(format!("read checkpoint object: {error}")));
            }
        };
        let kind = SFlag::from_bits_truncate(
            fstat(&descriptor)
                .map_err(|error| CheckpointError::new(format!("inspect checkpoint object: {error}")))?
                .st_mode,
        );
        if !kind.contains(SFlag::S_IFREG) {
            return Err(CheckpointError::new("checkpoint object is not a regular file"));
        }
        let mut bytes = Vec::new();
        std::fs::File::from(descriptor)
            .read_to_end(&mut bytes)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        Ok(Some(bytes))
    }

    #[cfg(unix)]
    pub(super) fn open_directory(
        root: &std::os::fd::OwnedFd,
        name: &str,
    ) -> Result<std::os::fd::OwnedFd, CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;

        openat(
            root,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| CheckpointError::new(format!("open checkpoint directory: {error}")))
    }

    #[cfg(unix)]
    pub(super) fn regular_exists(root: &std::os::fd::OwnedFd, name: &str) -> Result<bool, CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};

        let descriptor = match openat(
            root,
            name,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(nix::errno::Errno::ENOENT) => return Ok(false),
            Err(error) => {
                return Err(CheckpointError::new(format!("inspect checkpoint object: {error}")));
            }
        };
        let mode = fstat(&descriptor)
            .map_err(|error| CheckpointError::new(format!("inspect checkpoint object: {error}")))?
            .st_mode;
        Ok(SFlag::from_bits_truncate(mode).contains(SFlag::S_IFREG))
    }

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

    #[cfg(not(unix))]
    pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<(), CheckpointError> {
        let parent = path
            .parent()
            .ok_or_else(|| CheckpointError::new("checkpoint object has no parent"))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| CheckpointError::new(format!("create checkpoint object directory: {error}")))?;
        let result = (|| {
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            file.persist(path).map_err(|error| error.error)?;
            #[cfg(unix)]
            std::fs::File::open(parent)?.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        result.map_err(|error| CheckpointError::new(format!("replace checkpoint object: {error}")))
    }

    #[cfg(unix)]
    pub(super) fn replace_at(root: &std::os::fd::OwnedFd, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
        match Self::replace_at_outcome(root, name, bytes)? {
            PublicationOutcome::Durable => Ok(()),
            PublicationOutcome::PublishedNotDurable(error) => Err(error),
        }
    }

    #[cfg(unix)]
    pub(super) fn replace_at_outcome(
        root: &std::os::fd::OwnedFd,
        name: &str,
        bytes: &[u8],
    ) -> Result<PublicationOutcome, CheckpointError> {
        use nix::fcntl::{OFlag, openat, renameat};
        use nix::sys::stat::Mode;
        use std::os::fd::AsFd as _;

        let (directory, leaf) = Self::parent(root, name, true)?;
        let temporary = format!(
            ".checkpoint-{}-{}",
            std::process::id(),
            GENERATION.fetch_add(1, Ordering::Relaxed)
        );
        let descriptor = openat(
            &directory,
            temporary.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|error| CheckpointError::new(format!("create checkpoint replacement: {error}")))?;
        let prepared = (|| {
            let mut file = std::fs::File::from(descriptor);
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        if let Err(error) = prepared {
            let _ = nix::unistd::unlinkat(&directory, temporary.as_str(), nix::unistd::UnlinkatFlags::NoRemoveDir);
            return Err(CheckpointError::new(format!("prepare checkpoint replacement: {error}")));
        }
        if let Err(error) = renameat(&directory, temporary.as_str(), &directory, leaf.as_os_str()) {
            let _ = nix::unistd::unlinkat(&directory, temporary.as_str(), nix::unistd::UnlinkatFlags::NoRemoveDir);
            return Err(CheckpointError::new(format!("publish checkpoint replacement: {error}")));
        }
        Ok(Self::publication_after_rename(nix::unistd::fsync(directory.as_fd())))
    }

    #[cfg(unix)]
    pub(super) fn publication_after_rename(result: Result<(), nix::errno::Errno>) -> PublicationOutcome {
        result.map_or_else(
            |error| {
                PublicationOutcome::PublishedNotDurable(CheckpointError::published(format!(
                    "checkpoint replacement was published but its directory sync failed: {error}"
                )))
            },
            |()| PublicationOutcome::Durable,
        )
    }

    #[cfg(unix)]
    pub(super) fn sync_tree(directory: std::os::fd::OwnedFd) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};
        use std::os::fd::AsFd as _;

        let mut directory = nix::dir::Dir::from_fd(directory)
            .map_err(|error| CheckpointError::new(format!("read checkpoint generation: {error}")))?;
        let names = directory
            .iter()
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name().to_owned())
                    .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for name in names {
            let name = name.as_c_str();
            if name == c"." || name == c".." {
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
            if kind.contains(SFlag::S_IFDIR) {
                Self::sync_tree(descriptor)?;
            } else if !kind.contains(SFlag::S_IFREG) {
                return Err(CheckpointError::new(
                    "checkpoint generation contains a non-regular object",
                ));
            }
        }
        nix::unistd::fsync(directory.as_fd())
            .map_err(|error| CheckpointError::new(format!("sync checkpoint generation: {error}")))
    }

    #[cfg(unix)]
    pub(super) fn remove_tree_at(root: &std::os::fd::OwnedFd, name: &str) -> Result<(), CheckpointError> {
        Self::remove_tree_at_until(root, name, None)
    }

    #[cfg(unix)]
    pub(super) fn remove_tree_at_until(
        root: &std::os::fd::OwnedFd,
        name: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;

        Self::check_deadline(deadline)?;
        let directory = match openat(
            root,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(nix::errno::Errno::ENOENT) => return Ok(()),
            Err(error) => {
                return Err(CheckpointError::new(format!(
                    "open checkpoint staging generation: {error}"
                )));
            }
        };
        Self::clear_directory_until(directory, deadline)?;
        Self::check_deadline(deadline)?;
        nix::unistd::unlinkat(root, name, nix::unistd::UnlinkatFlags::RemoveDir)
            .map_err(|error| CheckpointError::new(format!("remove checkpoint staging generation: {error}")))
    }

    #[cfg(unix)]
    fn clear_directory_until(
        directory: std::os::fd::OwnedFd,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};

        Self::check_deadline(deadline)?;
        let mut entries = nix::dir::Dir::from_fd(directory)
            .map_err(|error| CheckpointError::new(format!("read checkpoint staging generation: {error}")))?;
        let names = entries
            .iter()
            .map(|entry| {
                Self::check_deadline(deadline)?;
                entry
                    .map(|entry| entry.file_name().to_owned())
                    .map_err(|error| CheckpointError::new(format!("read checkpoint staging object: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for child in names {
            Self::check_deadline(deadline)?;
            let child = child.as_c_str();
            if child == c"." || child == c".." {
                continue;
            }
            let descriptor = openat(
                &entries,
                child,
                OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
                Mode::empty(),
            );
            let directory = descriptor.as_ref().is_ok_and(|descriptor| {
                fstat(descriptor).is_ok_and(|status| SFlag::from_bits_truncate(status.st_mode).contains(SFlag::S_IFDIR))
            });
            if directory {
                Self::clear_directory_until(
                    descriptor
                        .map_err(|error| CheckpointError::new(format!("open checkpoint staging directory: {error}")))?,
                    deadline,
                )?;
                Self::check_deadline(deadline)?;
                nix::unistd::unlinkat(&entries, child, nix::unistd::UnlinkatFlags::RemoveDir)
                    .map_err(|error| CheckpointError::new(format!("remove checkpoint staging directory: {error}")))?;
            } else {
                Self::check_deadline(deadline)?;
                nix::unistd::unlinkat(&entries, child, nix::unistd::UnlinkatFlags::NoRemoveDir)
                    .map_err(|error| CheckpointError::new(format!("remove checkpoint staging object: {error}")))?;
            }
        }
        Self::check_deadline(deadline)?;
        Ok(())
    }
}
