use std::fmt;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Failure from durable checkpoint object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointError {
    message: String,
}

impl CheckpointError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CheckpointError {}

/// One complete, named process-tree checkpoint image.
pub trait CheckpointImage: Send + Sync {
    /// Stores one object in the unpublished checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage or object-name failure.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError>;

    /// Reads one object from the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage, object-name, or missing-object failure.
    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError>;

    /// Lists objects in the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn list(&self) -> Result<Vec<String>, CheckpointError>;

    /// Publishes a complete generation after its manifest is durable.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        self.put("MANIFEST", manifest)
    }
}

/// Opens checkpoint images by stable container generation namespace.
pub trait CheckpointImages: Send + Sync {
    /// Opens one isolated checkpoint generation stream.
    ///
    /// # Errors
    /// Returns an invalid-namespace or storage failure.
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError>;
}

pub(crate) struct DirectoryImages {
    #[cfg(not(unix))]
    root: PathBuf,
    #[cfg(unix)]
    directory: Arc<std::os::fd::OwnedFd>,
}

impl DirectoryImages {
    pub(crate) fn open(root: PathBuf) -> Result<Self, CheckpointError> {
        std::fs::create_dir_all(&root)
            .map_err(|error| CheckpointError::new(format!("create checkpoint root: {error}")))?;
        #[cfg(unix)]
        let directory = {
            use nix::fcntl::{OFlag, open};
            use nix::sys::stat::Mode;
            Arc::new(
                open(
                    &root,
                    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| CheckpointError::new(format!("open checkpoint root: {error}")))?,
            )
        };
        Ok(Self {
            #[cfg(not(unix))]
            root,
            #[cfg(unix)]
            directory,
        })
    }

    #[cfg(unix)]
    fn open_held(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, mkdirat};

        match mkdirat(&*self.directory, namespace, Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(error) => {
                return Err(CheckpointError::new(format!("create checkpoint image: {error}")));
            }
        }
        let directory = openat(
            &*self.directory,
            namespace,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| CheckpointError::new(format!("open checkpoint image: {error}")))?;
        let current_pointer = DirectoryImage::read_optional(&directory, "current")?;
        let (current, base) = match current_pointer {
            Some(bytes) => {
                let generation = std::str::from_utf8(&bytes)
                    .map_err(|_| CheckpointError::new("checkpoint current generation is not UTF-8"))?;
                if !DirectoryImage::valid_generation(generation) {
                    return Err(CheckpointError::new("checkpoint current generation is invalid"));
                }
                let held = DirectoryImage::open_directory(&directory, generation)
                    .map_err(|_| CheckpointError::new("checkpoint current generation is incomplete"))?;
                if !DirectoryImage::regular_exists(&held, "MANIFEST")? {
                    return Err(CheckpointError::new("checkpoint current generation is incomplete"));
                }
                (Some(DirectoryGeneration::Named(generation.to_owned())), Some(bytes))
            }
            None => {
                let current =
                    DirectoryImage::regular_exists(&directory, "MANIFEST")?.then_some(DirectoryGeneration::Namespace);
                (current, None)
            }
        };
        let generation = DirectoryImage::generation();
        Ok(Arc::new(DirectoryImage {
            directory,
            state: Mutex::new(DirectoryImageState {
                current,
                base,
                generation,
            }),
        }))
    }
}

impl CheckpointImages for DirectoryImages {
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError> {
        if namespace.is_empty()
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(CheckpointError::new("invalid checkpoint namespace"));
        }
        #[cfg(unix)]
        return self.open_held(namespace);
        #[cfg(not(unix))]
        {
            let root = self.root.join(namespace);
            std::fs::create_dir_all(&root)
                .map_err(|error| CheckpointError::new(format!("create checkpoint image: {error}")))?;
            let current_pointer = std::fs::read(root.join("current"));
            let (current, base) = match current_pointer {
                Ok(bytes) => {
                    let generation = std::str::from_utf8(&bytes)
                        .map_err(|_| CheckpointError::new("checkpoint current generation is not UTF-8"))?;
                    if !DirectoryImage::valid_generation(generation) {
                        return Err(CheckpointError::new("checkpoint current generation is invalid"));
                    }
                    let path = root.join(generation);
                    if !path.is_dir() || !path.join("MANIFEST").is_file() {
                        return Err(CheckpointError::new("checkpoint current generation is incomplete"));
                    }
                    (Some(DirectoryGeneration::Named(generation.to_owned())), Some(bytes))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
                    root.join("MANIFEST")
                        .is_file()
                        .then_some(DirectoryGeneration::Namespace),
                    None,
                ),
                Err(error) => {
                    return Err(CheckpointError::new(format!(
                        "read checkpoint current generation: {error}"
                    )));
                }
            };
            let generation = DirectoryImage::generation();
            Ok(Arc::new(DirectoryImage {
                root: root.clone(),
                state: Mutex::new(DirectoryImageState {
                    current,
                    base,
                    generation,
                }),
                #[cfg(unix)]
                directory: unreachable!(),
            }))
        }
    }
}

struct DirectoryImage {
    #[cfg(not(unix))]
    root: PathBuf,
    #[cfg(unix)]
    directory: std::os::fd::OwnedFd,
    state: Mutex<DirectoryImageState>,
}

#[derive(Clone)]
enum DirectoryGeneration {
    Namespace,
    Named(String),
}

struct DirectoryImageState {
    current: Option<DirectoryGeneration>,
    base: Option<Vec<u8>>,
    generation: String,
}

impl Drop for DirectoryImage {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            #[cfg(unix)]
            let _ = Self::remove_tree_at(&self.directory, &state.generation);
            #[cfg(not(unix))]
            let _ = std::fs::remove_dir_all(self.root.join(&state.generation));
        }
    }
}

impl DirectoryImage {
    fn generation() -> String {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        format!(
            "generation-{time}-{}-{}",
            std::process::id(),
            GENERATION.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn valid_generation(generation: &str) -> bool {
        generation.starts_with("generation-")
            && generation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }

    fn state(&self) -> Result<MutexGuard<'_, DirectoryImageState>, CheckpointError> {
        self.state
            .lock()
            .map_err(|_| CheckpointError::new("checkpoint generation lock is poisoned"))
    }

    #[cfg(unix)]
    fn hold_generation(&self, generation: &DirectoryGeneration) -> Result<std::os::fd::OwnedFd, CheckpointError> {
        match generation {
            DirectoryGeneration::Namespace => Self::open_directory(&self.directory, "."),
            DirectoryGeneration::Named(name) => Self::open_directory(&self.directory, name),
        }
    }

    #[cfg(not(unix))]
    fn generation_path(&self, generation: &DirectoryGeneration) -> PathBuf {
        match generation {
            DirectoryGeneration::Namespace => self.root.clone(),
            DirectoryGeneration::Named(name) => self.root.join(name),
        }
    }

    fn path(root: &Path, name: &str) -> Result<PathBuf, CheckpointError> {
        let path = Path::new(name);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(value) if !value.is_empty()))
        {
            return Err(CheckpointError::new(format!(
                "invalid checkpoint object name: {name:?}"
            )));
        }
        Ok(root.join(path))
    }

    #[cfg(unix)]
    fn parent(
        root: &std::os::fd::OwnedFd,
        name: &str,
        create: bool,
    ) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, mkdirat};
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
            match openat(&directory, component.as_os_str(), flags, Mode::empty()) {
                Ok(child) => directory = child,
                Err(nix::errno::Errno::ENOENT) if create => {
                    match mkdirat(&directory, component.as_os_str(), Mode::from_bits_truncate(0o700)) {
                        Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
                        Err(error) => {
                            return Err(CheckpointError::new(format!(
                                "create checkpoint object directory: {error}"
                            )));
                        }
                    }
                    directory = openat(&directory, component.as_os_str(), flags, Mode::empty())
                        .map_err(|error| CheckpointError::new(format!("open checkpoint object directory: {error}")))?;
                }
                Err(error) => {
                    return Err(CheckpointError::new(format!(
                        "open checkpoint object directory: {error}"
                    )));
                }
            }
        }
        Ok((directory, leaf))
    }

    #[cfg(unix)]
    fn read(root: &std::os::fd::OwnedFd, name: &str) -> Result<Vec<u8>, CheckpointError> {
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
    fn read_optional(root: &std::os::fd::OwnedFd, name: &str) -> Result<Option<Vec<u8>>, CheckpointError> {
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
    fn open_directory(root: &std::os::fd::OwnedFd, name: &str) -> Result<std::os::fd::OwnedFd, CheckpointError> {
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
    fn regular_exists(root: &std::os::fd::OwnedFd, name: &str) -> Result<bool, CheckpointError> {
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
    fn collect(
        root: &Path,
        directory: &Path,
        exclude_generation_metadata: bool,
        objects: &mut Vec<String>,
    ) -> Result<(), CheckpointError> {
        for entry in std::fs::read_dir(directory)
            .map_err(|error| CheckpointError::new(format!("list checkpoint objects: {error}")))?
        {
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
            let kind = entry
                .file_type()
                .map_err(|error| CheckpointError::new(error.to_string()))?;
            if kind.is_symlink() {
                return Err(CheckpointError::new("checkpoint image contains a symbolic link"));
            }
            if kind.is_dir() {
                Self::collect(root, &entry.path(), exclude_generation_metadata, objects)?;
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
        Ok(())
    }

    #[cfg(unix)]
    fn collect_held(
        directory: std::os::fd::OwnedFd,
        prefix: &str,
        exclude_generation_metadata: bool,
        objects: &mut Vec<String>,
    ) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};

        let mut directory = nix::dir::Dir::from_fd(directory)
            .map_err(|error| CheckpointError::new(format!("list checkpoint objects: {error}")))?;
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
                Self::collect_held(descriptor, &relative, exclude_generation_metadata, objects)?;
            } else if kind.contains(SFlag::S_IFREG) {
                objects.push(relative);
            } else {
                return Err(CheckpointError::new("checkpoint image contains a non-regular object"));
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn replace(path: &Path, bytes: &[u8]) -> Result<(), CheckpointError> {
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
    fn replace_at(root: &std::os::fd::OwnedFd, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
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
        let result = (|| {
            let mut file = std::fs::File::from(descriptor);
            file.write_all(bytes)?;
            file.sync_all()?;
            renameat(&directory, temporary.as_str(), &directory, leaf.as_os_str()).map_err(std::io::Error::from)?;
            nix::unistd::fsync(directory.as_fd()).map_err(std::io::Error::from)
        })();
        if result.is_err() {
            let _ = nix::unistd::unlinkat(&directory, temporary.as_str(), nix::unistd::UnlinkatFlags::NoRemoveDir);
        }
        result.map_err(|error| CheckpointError::new(format!("replace checkpoint object: {error}")))
    }

    #[cfg(unix)]
    fn sync_tree(directory: std::os::fd::OwnedFd) -> Result<(), CheckpointError> {
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
    fn remove_tree_at(root: &std::os::fd::OwnedFd, name: &str) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::Mode;

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
        Self::clear_directory(directory)?;
        nix::unistd::unlinkat(root, name, nix::unistd::UnlinkatFlags::RemoveDir)
            .map_err(|error| CheckpointError::new(format!("remove checkpoint staging generation: {error}")))
    }

    #[cfg(unix)]
    fn clear_directory(directory: std::os::fd::OwnedFd) -> Result<(), CheckpointError> {
        use nix::fcntl::{OFlag, openat};
        use nix::sys::stat::{Mode, SFlag, fstat};

        let mut entries = nix::dir::Dir::from_fd(directory)
            .map_err(|error| CheckpointError::new(format!("read checkpoint staging generation: {error}")))?;
        let names = entries
            .iter()
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name().to_owned())
                    .map_err(|error| CheckpointError::new(format!("read checkpoint staging object: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for child in names {
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
                Self::clear_directory(
                    descriptor
                        .map_err(|error| CheckpointError::new(format!("open checkpoint staging directory: {error}")))?,
                )?;
                nix::unistd::unlinkat(&entries, child, nix::unistd::UnlinkatFlags::RemoveDir)
                    .map_err(|error| CheckpointError::new(format!("remove checkpoint staging directory: {error}")))?;
            } else {
                nix::unistd::unlinkat(&entries, child, nix::unistd::UnlinkatFlags::NoRemoveDir)
                    .map_err(|error| CheckpointError::new(format!("remove checkpoint staging object: {error}")))?;
            }
        }
        Ok(())
    }
}

impl CheckpointImage for DirectoryImage {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
        let state = self.state()?;
        #[cfg(unix)]
        return Self::replace_at(&self.directory, &format!("{}/{name}", state.generation), bytes);
        #[cfg(not(unix))]
        Self::replace(&Self::path(&self.root.join(&state.generation), name)?, bytes)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        let state = self.state()?;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?;
        #[cfg(unix)]
        return Self::read(&self.hold_generation(current)?, name);
        #[cfg(not(unix))]
        std::fs::read(Self::path(&self.generation_path(current), name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))
    }

    fn list(&self) -> Result<Vec<String>, CheckpointError> {
        let state = self.state()?;
        let Some(current) = &state.current else {
            return Ok(Vec::new());
        };
        let mut objects = Vec::new();
        #[cfg(unix)]
        {
            Self::collect_held(
                self.hold_generation(current)?,
                "",
                matches!(current, DirectoryGeneration::Namespace),
                &mut objects,
            )?;
        }
        #[cfg(not(unix))]
        {
            let current = self.generation_path(current);
            Self::collect(
                &current,
                &current,
                matches!(state.current, Some(DirectoryGeneration::Namespace)),
                &mut objects,
            )?;
        }
        objects.sort();
        Ok(objects)
    }

    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        let mut state = self.state()?;
        #[cfg(unix)]
        Self::replace_at(&self.directory, &format!("{}/MANIFEST", state.generation), manifest)?;
        #[cfg(not(unix))]
        Self::replace(&self.root.join(&state.generation).join("MANIFEST"), manifest)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let staging = Self::open_directory(&self.directory, &state.generation)?;
            Self::sync_tree(staging)?;
            nix::unistd::fsync(self.directory.as_fd())
                .map_err(|error| CheckpointError::new(format!("sync checkpoint namespace: {error}")))?;
        }
        #[cfg(unix)]
        let lock = {
            use nix::fcntl::{OFlag, openat};
            use nix::sys::stat::{Mode, SFlag, fstat};
            let descriptor = openat(
                &self.directory,
                ".publication.lock",
                OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|error| CheckpointError::new(format!("open checkpoint publication lock: {error}")))?;
            let kind = SFlag::from_bits_truncate(
                fstat(&descriptor)
                    .map_err(|error| CheckpointError::new(format!("inspect checkpoint publication lock: {error}")))?
                    .st_mode,
            );
            if !kind.contains(SFlag::S_IFREG) {
                return Err(CheckpointError::new(
                    "checkpoint publication lock is not a regular file",
                ));
            }
            std::fs::File::from(descriptor)
        };
        #[cfg(not(unix))]
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(".publication.lock"))
            .map_err(|error| CheckpointError::new(format!("open checkpoint publication lock: {error}")))?;
        fs2::FileExt::lock_exclusive(&lock)
            .map_err(|error| CheckpointError::new(format!("lock checkpoint publication: {error}")))?;
        #[cfg(unix)]
        let published = Self::read_optional(&self.directory, "current")?;
        #[cfg(not(unix))]
        let published = match std::fs::read(self.root.join("current")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(CheckpointError::new(format!(
                    "read checkpoint current generation: {error}"
                )));
            }
        };
        if published != state.base {
            return Err(CheckpointError::new(
                "checkpoint generation changed while capture was in progress",
            ));
        }
        let generation = state.generation.as_bytes().to_vec();
        #[cfg(unix)]
        Self::replace_at(&self.directory, "current", &generation)?;
        #[cfg(not(unix))]
        Self::replace(&self.root.join("current"), &generation)?;
        state.current = Some(DirectoryGeneration::Named(state.generation.clone()));
        state.base = Some(generation);
        state.generation = Self::generation();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckpointImages as _, DirectoryImages};

    #[test]
    fn incomplete_capture_cannot_modify_committed_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let images = DirectoryImages::open(temporary.path().join("checkpoints")).unwrap();
        let first = images.open("container").unwrap();
        first.put("proc.1/pages", b"first").unwrap();
        first.commit(b"manifest-one").unwrap();

        let failed = images.open("container").unwrap();
        failed.put("proc.1/pages", b"torn-second").unwrap();

        let restored = images.open("container").unwrap();
        assert_eq!(restored.get("proc.1/pages").unwrap(), b"first");
        assert_eq!(restored.get("MANIFEST").unwrap(), b"manifest-one");
        assert_eq!(restored.list().unwrap(), ["MANIFEST", "proc.1/pages"]);
    }

    #[test]
    fn stale_capture_cannot_replace_a_newer_committed_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let first_process = DirectoryImages::open(root.clone()).unwrap();
        let second_process = DirectoryImages::open(root).unwrap();

        let older = first_process.open("container").unwrap();
        let newer = second_process.open("container").unwrap();
        older.put("state", b"older").unwrap();
        newer.put("state", b"newer").unwrap();

        newer.commit(b"newer-manifest").unwrap();
        assert!(older.commit(b"older-manifest").is_err());

        let restored = first_process.open("container").unwrap();
        assert_eq!(restored.get("state").unwrap(), b"newer");
        assert_eq!(restored.get("MANIFEST").unwrap(), b"newer-manifest");
    }

    #[test]
    fn corrupt_current_pointer_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let images = DirectoryImages::open(root.clone()).unwrap();
        let image = images.open("container").unwrap();
        image.put("state", b"complete").unwrap();
        image.commit(b"manifest").unwrap();

        std::fs::write(root.join("container/current"), b"../other").unwrap();
        assert!(images.open("container").is_err());
    }

    #[test]
    fn legacy_flat_generation_remains_restorable_until_replaced() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let namespace = root.join("container");
        std::fs::create_dir_all(namespace.join("proc.1")).unwrap();
        std::fs::write(namespace.join("MANIFEST"), b"legacy-manifest").unwrap();
        std::fs::write(namespace.join("proc.1/pages"), b"legacy-pages").unwrap();

        let images = DirectoryImages::open(root).unwrap();
        let image = images.open("container").unwrap();
        assert_eq!(image.get("MANIFEST").unwrap(), b"legacy-manifest");
        assert_eq!(image.get("proc.1/pages").unwrap(), b"legacy-pages");
        assert_eq!(image.list().unwrap(), ["MANIFEST", "proc.1/pages"]);

        image.put("proc.1/pages", b"replacement-pages").unwrap();
        assert_eq!(image.get("proc.1/pages").unwrap(), b"legacy-pages");
        image.commit(b"replacement-manifest").unwrap();
        assert_eq!(image.get("proc.1/pages").unwrap(), b"replacement-pages");
    }

    #[cfg(unix)]
    #[test]
    fn object_access_never_follows_a_symlink_outside_the_generation() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("pages"), b"foreign").unwrap();

        let images = DirectoryImages::open(root.clone()).unwrap();
        let image = images.open("container").unwrap();
        image.put("seed", b"seed").unwrap();
        let namespace = root.join("container");
        let staging = std::fs::read_dir(&namespace)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("generation-"))
            .unwrap()
            .path();
        symlink(&outside, staging.join("proc.1")).unwrap();

        assert!(image.put("proc.1/pages", b"escaped").is_err());
        assert_eq!(std::fs::read(outside.join("pages")).unwrap(), b"foreign");

        std::fs::remove_file(staging.join("proc.1")).unwrap();
        image.put("proc.1/pages", b"inside").unwrap();
        image.commit(b"manifest").unwrap();
        std::fs::remove_dir_all(staging.join("proc.1")).unwrap();
        symlink(&outside, staging.join("proc.1")).unwrap();

        assert!(image.get("proc.1/pages").is_err());
        assert!(image.list().is_err());
        assert_eq!(std::fs::read(outside.join("pages")).unwrap(), b"foreign");
    }

    #[cfg(unix)]
    #[test]
    fn publication_lock_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let outside = temporary.path().join("outside-lock");
        std::fs::write(&outside, b"foreign").unwrap();
        let images = DirectoryImages::open(root.clone()).unwrap();
        let image = images.open("container").unwrap();
        image.put("state", b"candidate").unwrap();
        symlink(&outside, root.join("container/.publication.lock")).unwrap();

        assert!(image.commit(b"manifest").is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"foreign");
        assert!(!root.join("container/current").exists());
    }

    #[cfg(unix)]
    #[test]
    fn held_root_and_namespace_ignore_path_replacement() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let held_root = temporary.path().join("held-root");
        let held_namespace = held_root.join("held-container");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let images = DirectoryImages::open(root.clone()).unwrap();
        std::fs::rename(&root, &held_root).unwrap();
        symlink(&outside, &root).unwrap();
        let image = images.open("container").unwrap();
        std::fs::rename(held_root.join("container"), &held_namespace).unwrap();
        symlink(&outside, held_root.join("container")).unwrap();

        image.put("state", b"inside").unwrap();
        image.commit(b"manifest").unwrap();
        assert!(outside.read_dir().unwrap().next().is_none());
        assert!(held_namespace.join("current").is_file());
        assert_eq!(image.get("state").unwrap(), b"inside");
    }

    #[cfg(unix)]
    #[test]
    fn current_and_generation_symlinks_fail_closed() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("current"), b"generation-foreign").unwrap();
        std::fs::write(outside.join("MANIFEST"), b"foreign-manifest").unwrap();

        let images = DirectoryImages::open(root.clone()).unwrap();
        let image = images.open("container").unwrap();
        image.put("state", b"candidate").unwrap();
        let namespace = root.join("container");
        symlink(outside.join("current"), namespace.join("current")).unwrap();
        assert!(image.commit(b"manifest").is_err());
        assert_eq!(std::fs::read(outside.join("current")).unwrap(), b"generation-foreign");
        std::fs::remove_file(namespace.join("current")).unwrap();

        image.commit(b"manifest").unwrap();
        let generation = std::str::from_utf8(&std::fs::read(namespace.join("current")).unwrap())
            .unwrap()
            .to_owned();
        std::fs::rename(namespace.join(&generation), namespace.join("held-generation")).unwrap();
        symlink(&outside, namespace.join(generation)).unwrap();
        assert!(image.get("state").is_err());
        assert!(image.list().is_err());
        assert_eq!(std::fs::read(outside.join("MANIFEST")).unwrap(), b"foreign-manifest");
    }

    #[cfg(unix)]
    #[test]
    fn failed_publication_preserves_current_and_cleans_staging() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkpoints");
        let images = DirectoryImages::open(root.clone()).unwrap();
        let first = images.open("container").unwrap();
        first.put("state", b"first").unwrap();
        first.commit(b"manifest-one").unwrap();
        drop(first);

        let failed = images.open("container").unwrap();
        failed.put("state", b"second").unwrap();
        let namespace = root.join("container");
        std::fs::set_permissions(&namespace, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(failed.commit(b"manifest-two").is_err());
        std::fs::set_permissions(&namespace, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(failed);

        let restored = images.open("container").unwrap();
        assert_eq!(restored.get("state").unwrap(), b"first");
        assert_eq!(restored.get("MANIFEST").unwrap(), b"manifest-one");
        let generations = std::fs::read_dir(namespace)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .count();
        assert_eq!(generations, 1, "failed staging generation leaked");
    }
}
