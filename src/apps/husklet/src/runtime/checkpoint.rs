use hl_container::{CheckpointError, CheckpointImage, CheckpointImages};
use hl_ws::{Directory, Key, Namespace, Storage};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Workspace-owned checkpoint generations with atomic manifest publication.
pub(super) struct WorkspaceCheckpoints {
    storage: Directory,
}

impl WorkspaceCheckpoints {
    pub(super) fn open(workspace: &std::path::Path) -> Result<Self, CheckpointError> {
        Directory::open(workspace)
            .map(|storage| Self { storage })
            .map_err(Self::error)
    }

    fn error(error: impl std::fmt::Display) -> CheckpointError {
        CheckpointError::new(error.to_string())
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
}

impl CheckpointImages for WorkspaceCheckpoints {
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError> {
        let root = Key::parse("checkpoints")
            .and_then(|key| key.join(namespace))
            .map_err(Self::error)?;
        let current_key = root.join("current").map_err(Self::error)?;
        let current = match self.storage.get(&current_key) {
            Ok(bytes) => {
                let generation = std::str::from_utf8(&bytes).map_err(Self::error)?.trim();
                Some(Namespace::new(
                    self.storage.clone(),
                    root.join(generation).map_err(Self::error)?,
                ))
            }
            Err(hl_ws::storage::Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(error) => return Err(Self::error(error)),
        };
        let generation = Self::generation();
        let staging = Namespace::new(
            self.storage.clone(),
            root.join(&generation).map_err(Self::error)?,
        );
        Ok(Arc::new(WorkspaceImage {
            storage: self.storage.clone(),
            root,
            current_key,
            state: std::sync::Mutex::new(ImageState {
                current,
                staging,
                generation,
            }),
        }))
    }
}

struct WorkspaceImage {
    storage: Directory,
    root: Key,
    current_key: Key,
    state: std::sync::Mutex<ImageState>,
}

struct ImageState {
    current: Option<Namespace<Directory>>,
    staging: Namespace<Directory>,
    generation: String,
}

impl WorkspaceImage {
    fn key(name: &str) -> Result<Key, CheckpointError> {
        Key::parse(name).map_err(WorkspaceCheckpoints::error)
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, ImageState>, CheckpointError> {
        self.state
            .lock()
            .map_err(|_| CheckpointError::new("checkpoint generation lock is poisoned"))
    }
}

impl CheckpointImage for WorkspaceImage {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
        self.state()?
            .staging
            .put(&Self::key(name)?, bytes)
            .map_err(WorkspaceCheckpoints::error)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        self.state()?
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?
            .get(&Self::key(name)?)
            .map_err(WorkspaceCheckpoints::error)
    }

    fn list(&self) -> Result<Vec<String>, CheckpointError> {
        let state = self.state()?;
        let Some(current) = &state.current else {
            return Ok(Vec::new());
        };
        current
            .list(None)
            .map(|keys| {
                keys.into_iter()
                    .map(|key| key.as_str().to_owned())
                    .collect()
            })
            .map_err(WorkspaceCheckpoints::error)
    }

    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        let mut state = self.state()?;
        state
            .staging
            .put(&Self::key("MANIFEST")?, manifest)
            .and_then(|()| {
                self.storage
                    .put(&self.current_key, state.generation.as_bytes())
            })
            .map_err(WorkspaceCheckpoints::error)?;

        state.current = Some(state.staging.clone());
        state.generation = WorkspaceCheckpoints::generation();
        state.staging = Namespace::new(
            self.storage.clone(),
            self.root
                .join(&state.generation)
                .map_err(WorkspaceCheckpoints::error)?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_capture_never_replaces_current_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let first = images.open("container").unwrap();
        first.put("proc.1/pages", b"first").unwrap();
        first.commit(b"one").unwrap();

        let failed = images.open("container").unwrap();
        failed.put("proc.1/pages", b"incomplete").unwrap();

        let restored = images.open("container").unwrap();
        assert_eq!(restored.get("proc.1/pages").unwrap(), b"first");
        assert_eq!(restored.get("MANIFEST").unwrap(), b"one");
        assert_eq!(restored.list().unwrap(), ["MANIFEST", "proc.1/pages"]);
    }

    #[test]
    fn container_namespaces_are_isolated() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let alpha = images.open("alpha").unwrap();
        alpha.put("state", b"alpha").unwrap();
        alpha.commit(b"manifest").unwrap();
        let beta = images.open("beta").unwrap();
        beta.put("state", b"beta").unwrap();
        beta.commit(b"manifest").unwrap();

        assert_eq!(
            images.open("alpha").unwrap().get("state").unwrap(),
            b"alpha"
        );
        assert_eq!(images.open("beta").unwrap().get("state").unwrap(), b"beta");
    }

    #[test]
    fn repeated_capture_uses_a_fresh_unpublished_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let image = images.open("terminal").unwrap();
        image.put("proc.1/pages", b"first").unwrap();
        image.commit(b"manifest-one").unwrap();

        image.put("proc.1/pages", b"incomplete-second").unwrap();

        assert_eq!(image.get("proc.1/pages").unwrap(), b"first");
        assert_eq!(image.get("MANIFEST").unwrap(), b"manifest-one");

        image.put("proc.1/pages", b"second").unwrap();
        image.commit(b"manifest-two").unwrap();

        assert_eq!(image.get("proc.1/pages").unwrap(), b"second");
        assert_eq!(image.get("MANIFEST").unwrap(), b"manifest-two");
        let reopened = images.open("terminal").unwrap();
        assert_eq!(reopened.get("proc.1/pages").unwrap(), b"second");
        assert_eq!(reopened.get("MANIFEST").unwrap(), b"manifest-two");
    }
}
