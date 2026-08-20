//! # The `cfg(not(unix))` arms in this subtree compile in no configuration at all
//!
//! `filesystem.rs`, `filesystem/inventory.rs` and `checkpoint/directory{,/listing,
//! /transaction,/storage}.rs` hold 26 `#[cfg(not(unix))]` arms between them. Nothing
//! builds them. `aws-lc-sys` refuses the mingw target, so `hl-container` cannot be
//! cross-checked at a Windows target -- but that is only the outermost obstacle, and
//! removing it would not help, because THIS CRATE CANNOT BE CONFIGURED NON-UNIX:
//!
//!   * `Cargo.toml` lists `nix = "0.30"` in plain `[dependencies]`, not under
//!     `[target.'cfg(unix)'.dependencies]`, and `nix` does not build off Unix;
//!   * `src/config.rs` and `src/engine/spec.rs` import `std::os::unix::fs` with no
//!     `cfg` on them at all;
//!   * and the same is true one crate down -- `hl-images`, which this crate depends
//!     on unconditionally, also lists `nix = "0.30"` in plain `[dependencies]`. It is
//!     also where `aws-lc-sys` comes from, through `oci-client` and `reqwest`.
//!
//! So these are not arms awaiting a host to run on. They are unreachable in every
//! configuration the manifest permits, which is why nothing has ever type-checked
//! them and why no build oracle reports it.
//!
//! They can be type-checked by hand, and were, on `x86_64` Linux at 641d3f580: rewrite
//! every `cfg(unix)`/`cfg(not(unix))` in `src/` to a cfg name that is never set,
//! build with `RUSTFLAGS=--check-cfg=cfg(<name>)`, and `cargo check -p hl-container
//! --lib`. Selection was proved rather than assumed by planting `let _planted: u8 =
//! "...";` inside `listing.rs::collect`, which reddened at E0308 exactly there. The
//! result was 0 errors and 7 warnings, all of them exclusive to this arm: four unused
//! imports (`storage.rs` `GENERATION` and `Ordering`; `transaction.rs` `super::storage`
//! and `DirectoryImageState`), two dead items (`CheckpointError::published`,
//! `storage::PublicationOutcome`), and one that is a contract divergence rather than
//! tidiness -- `abort_state` below takes a `deadline` and its non-Unix arm never reads
//! it, so the bounded abort that the Unix arm gets from `remove_tree_at_until` is
//! simply unbounded there.
//!
//! What the technique proves is narrow, and the limit is worth stating plainly: it is
//! sound for cfg-graph consistency and blind to libc and ABI differences. It shows
//! these arms parse, name-resolve and type-check against Linux's `std`. It says
//! nothing about whether they are correct on Windows.
//!
//! Deciding what to do belongs to this crate's owner and is one of two things, both
//! larger than tidying the warnings: delete the 26 arms because `hl-container` is
//! Unix-only by construction, or make the crate genuinely portable -- which is not a
//! one-crate edit, because `hl-images` has to move first. Silencing the seven warnings
//! on its own would be the worst of the three: it would leave the arm looking
//! maintained while still building nowhere.

use super::{CheckpointError, CheckpointImage, CheckpointImages};
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

mod listing;
mod storage;
mod transaction;

static GENERATION: AtomicU64 = AtomicU64::new(0);
static TRANSACTION: AtomicU64 = AtomicU64::new(1);
static IMAGES: OnceLock<Mutex<HashMap<String, std::sync::Weak<DirectoryImage>>>> = OnceLock::new();

pub(crate) struct DirectoryImages {
    #[cfg(not(unix))]
    root: PathBuf,
    #[cfg(unix)]
    directory: Arc<std::os::fd::OwnedFd>,
    identity: String,
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
        #[cfg(unix)]
        let identity = {
            let metadata = nix::sys::stat::fstat(&*directory)
                .map_err(|error| CheckpointError::new(format!("inspect checkpoint root: {error}")))?;
            format!("{}:{}", metadata.st_dev, metadata.st_ino)
        };
        #[cfg(not(unix))]
        let identity = root
            .canonicalize()
            .map_err(|error| CheckpointError::new(format!("resolve checkpoint root: {error}")))?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            #[cfg(not(unix))]
            root: root.to_owned(),
            #[cfg(unix)]
            directory,
            identity,
        })
    }

    #[cfg(unix)]
    fn open_held(&self, namespace: &str) -> Result<Arc<DirectoryImage>, CheckpointError> {
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
                transaction: None,
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
        let key = format!("{}:{namespace}", self.identity);
        let mut images = IMAGES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| CheckpointError::new("checkpoint image cache is poisoned"))?;
        #[cfg(unix)]
        let inspected = self.open_held(namespace)?;
        if let Some(image) = images.get(&key).and_then(std::sync::Weak::upgrade) {
            #[cfg(unix)]
            {
                let inspected_state = inspected.state()?;
                let mut image_state = image.state()?;
                if image_state.transaction.is_none() {
                    image_state.current.clone_from(&inspected_state.current);
                    image_state.base.clone_from(&inspected_state.base);
                }
            }
            return Ok(image);
        }
        #[cfg(unix)]
        let image = inspected;
        #[cfg(not(unix))]
        let image = {
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
            Arc::new(DirectoryImage {
                root: root.clone(),
                state: Mutex::new(DirectoryImageState {
                    current,
                    base,
                    generation,
                    transaction: None,
                }),
                #[cfg(unix)]
                directory: unreachable!(),
            })
        };
        images.insert(key, Arc::downgrade(&image));
        Ok(image)
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
    transaction: Option<(NonZeroU64, std::time::Instant)>,
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

    fn abort_state<'a>(
        &self,
        mut state: MutexGuard<'a, DirectoryImageState>,
        deadline: std::time::Instant,
    ) -> Result<MutexGuard<'a, DirectoryImageState>, CheckpointError> {
        #[cfg(unix)]
        Self::remove_tree_at_until(&self.directory, &state.generation, Some(deadline))?;
        // `?` cannot carry an `io::Error` here: `CheckpointError` deliberately implements no
        // `From<io::Error>`, so every other non-POSIX arm in this subtree names the failure through
        // `CheckpointError::new`. This one did not, and nothing compiles it -- the mingw check builds
        // only `hl-native`/`hl-engine`/`engine`, and `hl-container` cannot cross-build at all because
        // `aws-lc-sys` refuses the target -- so the arm had never been type-checked on any host.
        #[cfg(not(unix))]
        std::fs::remove_dir_all(self.root.join(&state.generation))
            .or_else(|error| {
                (error.kind() == std::io::ErrorKind::NotFound)
                    .then_some(())
                    .ok_or(error)
            })
            .map_err(|error| CheckpointError::new(format!("remove checkpoint generation: {error}")))?;
        state.generation = Self::generation();
        state.transaction = None;
        Ok(state)
    }

    fn next_transaction() -> NonZeroU64 {
        loop {
            if let Some(transaction) = NonZeroU64::new(TRANSACTION.fetch_add(1, Ordering::Relaxed)) {
                return transaction;
            }
        }
    }

    fn validate_transaction(
        state: &DirectoryImageState,
        transaction: NonZeroU64,
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        let now = std::time::Instant::now();
        match state.transaction {
            Some((active, lease)) if active == transaction && now < deadline && now < lease => Ok(()),
            Some((active, _)) if active != transaction => {
                Err(CheckpointError::new("checkpoint transaction is not owned"))
            }
            _ => Err(CheckpointError::deadline()),
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

#[cfg(test)]
mod test;
