use super::{CheckpointError, CheckpointImage, CheckpointImages};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

mod storage;

static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) struct DirectoryImages {
    #[cfg(not(unix))]
    root: PathBuf,
    #[cfg(unix)]
    directory: Arc<std::os::fd::OwnedFd>,
}

impl DirectoryImages {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self, CheckpointError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)
            .map_err(|error| CheckpointError::new(format!("create checkpoint root: {error}")))?;
        #[cfg(unix)]
        let directory = {
            use nix::fcntl::{OFlag, open};
            use nix::sys::stat::Mode;
            Arc::new(
                open(
                    root,
                    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| CheckpointError::new(format!("open checkpoint root: {error}")))?,
            )
        };
        Ok(Self {
            #[cfg(not(unix))]
            root: root.to_owned(),
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
        let (current, base) = if let Some(bytes) = current_pointer {
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
        } else {
            let current =
                DirectoryImage::regular_exists(&directory, "MANIFEST")?.then_some(DirectoryGeneration::Namespace);
            (current, None)
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
            let result = Self::remove_tree_at(&self.directory, &state.generation);
            #[cfg(not(unix))]
            let result = std::fs::remove_dir_all(self.root.join(&state.generation)).or_else(|error| {
                (error.kind() == std::io::ErrorKind::NotFound)
                    .then_some(())
                    .ok_or(error)
            });
            Self::report_cleanup(&state.generation, result);
        }
    }
}

impl DirectoryImage {
    fn lock_publication(lock: &std::fs::File, deadline: Option<std::time::Instant>) -> Result<(), CheckpointError> {
        let Some(deadline) = deadline else {
            return fs2::FileExt::lock_exclusive(lock)
                .map_err(|error| CheckpointError::new(format!("lock checkpoint publication: {error}")));
        };
        loop {
            match fs2::FileExt::try_lock_exclusive(lock) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(std::time::Instant::now())
                            .min(std::time::Duration::from_millis(1)),
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(CheckpointError::deadline());
                }
                Err(error) => {
                    return Err(CheckpointError::new(format!("lock checkpoint publication: {error}")));
                }
            }
        }
    }

    fn report_cleanup<E: std::fmt::Display>(generation: &str, result: Result<(), E>) {
        if let Err(error) = result {
            hl_log::hl_error!(
                hl_log::tag::CHECKPOINT,
                "remove abandoned checkpoint generation generation={generation:?} error={error}"
            );
        }
    }

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

    fn state_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<MutexGuard<'_, DirectoryImageState>, CheckpointError> {
        loop {
            match self.state.try_lock() {
                Ok(state) => return Ok(state),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(CheckpointError::new("checkpoint generation lock is poisoned"));
                }
                Err(std::sync::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(std::time::Instant::now())
                            .min(std::time::Duration::from_millis(1)),
                    );
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(CheckpointError::deadline());
                }
            }
        }
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
}

impl CheckpointImage for DirectoryImage {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
        let state = self.state()?;
        #[cfg(unix)]
        return Self::replace_at(&self.directory, &format!("{}/{name}", state.generation), bytes);
        #[cfg(not(unix))]
        Self::replace(&Self::path(&self.root.join(&state.generation), name)?, bytes)
    }

    fn put_until(&self, name: &str, bytes: &[u8], deadline: std::time::Instant) -> Result<(), CheckpointError> {
        let state = self.state_until(deadline)?;
        #[cfg(unix)]
        Self::replace_at(&self.directory, &format!("{}/{name}", state.generation), bytes)?;
        #[cfg(not(unix))]
        Self::replace(&Self::path(&self.root.join(&state.generation), name)?, bytes)?;
        (std::time::Instant::now() < deadline)
            .then_some(())
            .ok_or_else(CheckpointError::deadline)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        let state = self.state()?;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?;
        #[cfg(unix)]
        let bytes = Self::read(&self.hold_generation(current)?, name)?;
        #[cfg(not(unix))]
        let bytes = std::fs::read(Self::path(&self.generation_path(current), name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        Ok(bytes)
    }

    fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, CheckpointError> {
        let state = self.state_until(deadline)?;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?;
        #[cfg(unix)]
        let bytes = Self::read(&self.hold_generation(current)?, name)?;
        #[cfg(not(unix))]
        let bytes = std::fs::read(Self::path(&self.generation_path(current), name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
        (std::time::Instant::now() < deadline)
            .then_some(bytes)
            .ok_or_else(CheckpointError::deadline)
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

    fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, CheckpointError> {
        let state = self.state_until(deadline)?;
        let Some(current) = &state.current else {
            return (std::time::Instant::now() < deadline)
                .then(Vec::new)
                .ok_or_else(CheckpointError::deadline);
        };
        let mut objects = Vec::new();
        #[cfg(unix)]
        Self::collect_held(
            self.hold_generation(current)?,
            "",
            matches!(current, DirectoryGeneration::Namespace),
            &mut objects,
        )?;
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
        (std::time::Instant::now() < deadline)
            .then_some(objects)
            .ok_or_else(CheckpointError::deadline)
    }

    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        self.commit_inner(manifest, None)
    }

    fn commit_until(&self, manifest: &[u8], deadline: std::time::Instant) -> Result<(), CheckpointError> {
        self.commit_inner(manifest, Some(deadline))
    }
}

impl DirectoryImage {
    #[cfg(unix)]
    fn finish_publication(
        state: &mut DirectoryImageState,
        generation: Vec<u8>,
        outcome: storage::PublicationOutcome,
    ) -> Result<(), CheckpointError> {
        state.current = Some(DirectoryGeneration::Named(state.generation.clone()));
        state.base = Some(generation);
        state.generation = Self::generation();
        match outcome {
            storage::PublicationOutcome::Durable => Ok(()),
            storage::PublicationOutcome::PublishedNotDurable(error) => Err(error),
        }
    }

    fn commit_inner(&self, manifest: &[u8], deadline: Option<std::time::Instant>) -> Result<(), CheckpointError> {
        let mut state = match deadline {
            Some(deadline) => self.state_until(deadline)?,
            None => self.state()?,
        };
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
        Self::lock_publication(&lock, deadline)?;
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
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(CheckpointError::deadline());
        }
        let generation = state.generation.as_bytes().to_vec();
        #[cfg(unix)]
        let publication = Self::replace_at_outcome(&self.directory, "current", &generation)?;
        #[cfg(not(unix))]
        {
            Self::replace(&self.root.join("current"), &generation)?;
            state.current = Some(DirectoryGeneration::Named(state.generation.clone()));
            state.base = Some(generation);
            state.generation = Self::generation();
        }
        #[cfg(unix)]
        return Self::finish_publication(&mut state, generation, publication);
        #[cfg(not(unix))]
        Ok(())
    }
}

#[cfg(test)]
mod test;
