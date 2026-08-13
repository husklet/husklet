use std::fmt;
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
    root: PathBuf,
}

impl DirectoryImages {
    pub(crate) fn open(root: PathBuf) -> Result<Self, CheckpointError> {
        std::fs::create_dir_all(&root)
            .map_err(|error| CheckpointError::new(format!("create checkpoint root: {error}")))?;
        Ok(Self { root })
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
                (Some(path), Some(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (root.join("MANIFEST").is_file().then_some(root.clone()), None)
            }
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
                staging: root.join(&generation),
                generation,
            }),
        }))
    }
}

struct DirectoryImage {
    root: PathBuf,
    state: Mutex<DirectoryImageState>,
}

struct DirectoryImageState {
    current: Option<PathBuf>,
    base: Option<Vec<u8>>,
    staging: PathBuf,
    generation: String,
}

impl Drop for DirectoryImage {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            let _ = std::fs::remove_dir_all(&state.staging);
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
            if entry
                .file_type()
                .map_err(|error| CheckpointError::new(error.to_string()))?
                .is_dir()
            {
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
    fn sync_tree(directory: &Path) -> Result<(), CheckpointError> {
        for entry in std::fs::read_dir(directory)
            .map_err(|error| CheckpointError::new(format!("read checkpoint generation: {error}")))?
        {
            let entry = entry.map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))?;
            if entry
                .file_type()
                .map_err(|error| CheckpointError::new(format!("inspect checkpoint object: {error}")))?
                .is_dir()
            {
                Self::sync_tree(&entry.path())?;
            }
        }
        std::fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| CheckpointError::new(format!("sync checkpoint generation: {error}")))
    }
}

impl CheckpointImage for DirectoryImage {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
        let state = self.state()?;
        Self::replace(&Self::path(&state.staging, name)?, bytes)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        let state = self.state()?;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?;
        std::fs::read(Self::path(current, name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))
    }

    fn list(&self) -> Result<Vec<String>, CheckpointError> {
        let state = self.state()?;
        let Some(current) = &state.current else {
            return Ok(Vec::new());
        };
        let mut objects = Vec::new();
        Self::collect(current, current, current == &self.root, &mut objects)?;
        objects.sort();
        Ok(objects)
    }

    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        let mut state = self.state()?;
        Self::replace(&state.staging.join("MANIFEST"), manifest)?;
        #[cfg(unix)]
        {
            Self::sync_tree(&state.staging)?;
            std::fs::File::open(&self.root)
                .and_then(|root| root.sync_all())
                .map_err(|error| CheckpointError::new(format!("sync checkpoint namespace: {error}")))?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(".publication.lock"))
            .map_err(|error| CheckpointError::new(format!("open checkpoint publication lock: {error}")))?;
        fs2::FileExt::lock_exclusive(&lock)
            .map_err(|error| CheckpointError::new(format!("lock checkpoint publication: {error}")))?;
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
        Self::replace(&self.root.join("current"), &generation)?;
        state.current = Some(state.staging.clone());
        state.base = Some(generation);
        state.generation = Self::generation();
        state.staging = self.root.join(&state.generation);
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
