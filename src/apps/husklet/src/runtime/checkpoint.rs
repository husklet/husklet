use hl_container::{CheckpointError, CheckpointImage, CheckpointImages};
use hl_ws::{Directory, Key, Namespace, Storage};
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static TRANSACTION: AtomicU64 = AtomicU64::new(1);
static IMAGES: OnceLock<Mutex<HashMap<String, std::sync::Weak<WorkspaceImage>>>> = OnceLock::new();

#[cfg(test)]
static REFRESH_BEFORE_STATE: OnceLock<Mutex<Option<(String, Arc<std::sync::Barrier>)>>> = OnceLock::new();
#[cfg(test)]
static WATCHED_CURRENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
#[cfg(test)]
static WATCHED_CURRENT_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn refresh_before_state(key: &str) {
    let configured = REFRESH_BEFORE_STATE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone();
    if let Some((watched, barrier)) = configured.filter(|(watched, _)| watched == key) {
        barrier.wait();
        barrier.wait();
    }
}

#[cfg(test)]
fn observe_current(generation: &str) {
    if WATCHED_CURRENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_deref()
        == Some(generation)
    {
        WATCHED_CURRENT_READS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Workspace-owned checkpoint generations with atomic manifest publication.
pub(super) struct WorkspaceCheckpoints {
    storage: Directory,
    identity: String,
}

impl WorkspaceCheckpoints {
    pub(super) fn open(workspace: &std::path::Path) -> Result<Self, CheckpointError> {
        let identity = workspace
            .canonicalize()
            .map_err(Self::error)?
            .to_string_lossy()
            .into_owned();
        Directory::open(workspace)
            .map(|storage| Self { storage, identity })
            .map_err(Self::error)
    }

    fn error(error: impl std::fmt::Display) -> CheckpointError {
        CheckpointError::new(error.to_string())
    }

    fn storage_error(error: hl_ws::storage::Error) -> CheckpointError {
        match error {
            hl_ws::storage::Error::Deadline => CheckpointError::deadline(),
            error => Self::error(error),
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

    fn current(
        &self,
        root: &Key,
        current_key: &Key,
    ) -> Result<Option<Namespace<Directory>>, CheckpointError> {
        match self.storage.get(current_key) {
            Ok(bytes) => {
                let generation = std::str::from_utf8(&bytes).map_err(Self::error)?.trim();
                #[cfg(test)]
                observe_current(generation);
                let current = Namespace::new(
                    self.storage.clone(),
                    root.join(generation).map_err(Self::error)?,
                );
                current
                    .get(&Key::parse("MANIFEST").map_err(Self::error)?)
                    .map_err(|error| Self::error(format!("checkpoint current generation is incomplete: {error}")))?;
                Ok(Some(current))
            }
            Err(hl_ws::storage::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Self::error(error)),
        }
    }
}

impl CheckpointImages for WorkspaceCheckpoints {
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError> {
        let key = format!("{}:{namespace}", self.identity);
        let root = Key::parse("checkpoints")
            .and_then(|key| key.join(namespace))
            .map_err(Self::error)?;
        let current_key = root.join("current").map_err(Self::error)?;
        let mut images = IMAGES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| CheckpointError::new("checkpoint image cache is poisoned"))?;
        if let Some(image) = images.get(&key).and_then(std::sync::Weak::upgrade) {
            // Do not hold the process-wide cache while reading storage. The image lock is the
            // refresh linearization point: an older read cannot arrive after a newer one and roll
            // the shared Arc back. An active capture owns its cached view and must not even inspect
            // an external pointer until that transaction has ended -- malformed external state is
            // not allowed to interrupt an already-open capture.
            drop(images);
            #[cfg(test)]
            refresh_before_state(&key);
            let mut state = image.state()?;
            if state.transaction.is_some() {
                drop(state);
                return Ok(image);
            }
            state.current = self.current(&root, &current_key)?;
            drop(state);
            return Ok(image);
        }
        let current = self.current(&root, &current_key)?;
        let generation = Self::generation();
        let staging = Namespace::new(self.storage.clone(), root.join(&generation).map_err(Self::error)?);
        let image = Arc::new(WorkspaceImage {
            storage: self.storage.clone(),
            root,
            current_key,
            state: std::sync::Mutex::new(ImageState {
                current,
                staging,
                generation,
                transaction: None,
            }),
        });
        images.insert(key, Arc::downgrade(&image));
        Ok(image)
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
    transaction: Option<(NonZeroU64, std::time::Instant)>,
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

    fn state_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<std::sync::MutexGuard<'_, ImageState>, CheckpointError> {
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
        mut state: std::sync::MutexGuard<'a, ImageState>,
        deadline: std::time::Instant,
    ) -> Result<std::sync::MutexGuard<'a, ImageState>, CheckpointError> {
        for key in state
            .staging
            .list_until(None, deadline)
            .map_err(WorkspaceCheckpoints::storage_error)?
        {
            state
                .staging
                .remove_until(&key, deadline)
                .map_err(WorkspaceCheckpoints::storage_error)?;
        }
        state.generation = WorkspaceCheckpoints::generation();
        state.staging = Namespace::new(
            self.storage.clone(),
            self.root.join(&state.generation).map_err(WorkspaceCheckpoints::error)?,
        );
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
        state: &ImageState,
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
}

impl CheckpointImage for WorkspaceImage {
    fn begin_until(&self, deadline: std::time::Instant) -> Result<NonZeroU64, CheckpointError> {
        if std::time::Instant::now() >= deadline {
            return Err(CheckpointError::deadline());
        }
        let mut state = self.state_until(deadline)?;
        if let Some((_, lease)) = state.transaction {
            if std::time::Instant::now() < lease {
                return Err(CheckpointError::busy());
            }
            state = self.abort_state(state, deadline)?;
        }
        let transaction = Self::next_transaction();
        state.transaction = Some((transaction, deadline));
        Ok(transaction)
    }

    fn put_until(
        &self,
        transaction: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        let state = self.state_until(deadline)?;
        Self::validate_transaction(&state, transaction, deadline)?;
        state
            .staging
            .put(&Self::key(name)?, bytes)
            .map_err(WorkspaceCheckpoints::error)?;
        (std::time::Instant::now() < deadline)
            .then_some(())
            .ok_or_else(CheckpointError::deadline)
    }

    fn abort_until(&self, transaction: NonZeroU64, deadline: std::time::Instant) -> Result<(), CheckpointError> {
        if std::time::Instant::now() >= deadline {
            return Err(CheckpointError::deadline());
        }
        let state = self.state_until(deadline)?;
        if !matches!(state.transaction, Some((active, _)) if active == transaction) {
            return Err(CheckpointError::new("checkpoint transaction is not owned"));
        }
        self.abort_state(state, deadline).map(|_| ())
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        self.state()?
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?
            .get(&Self::key(name)?)
            .map_err(WorkspaceCheckpoints::error)
    }

    fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, CheckpointError> {
        let bytes = self
            .state_until(deadline)?
            .current
            .as_ref()
            .ok_or_else(|| CheckpointError::new("checkpoint has no committed generation"))?
            .get(&Self::key(name)?)
            .map_err(WorkspaceCheckpoints::error)?;
        (std::time::Instant::now() < deadline)
            .then_some(bytes)
            .ok_or_else(CheckpointError::deadline)
    }

    fn list(&self) -> Result<Vec<String>, CheckpointError> {
        let state = self.state()?;
        let Some(current) = &state.current else {
            return Ok(Vec::new());
        };
        current
            .list(None)
            .map(|keys| keys.into_iter().map(|key| key.as_str().to_owned()).collect())
            .map_err(WorkspaceCheckpoints::error)
    }

    fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, CheckpointError> {
        let state = self.state_until(deadline)?;
        let Some(current) = &state.current else {
            return (std::time::Instant::now() < deadline)
                .then(Vec::new)
                .ok_or_else(CheckpointError::deadline);
        };
        let names = current
            .list_until(None, deadline)
            .map(|keys| keys.into_iter().map(|key| key.as_str().to_owned()).collect())
            .map_err(WorkspaceCheckpoints::storage_error)?;
        (std::time::Instant::now() < deadline)
            .then_some(names)
            .ok_or_else(CheckpointError::deadline)
    }

    fn commit_until(
        &self,
        transaction: NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        self.commit_inner(transaction, manifest, deadline)
    }
}

impl WorkspaceImage {
    fn commit_inner(
        &self,
        transaction: NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError> {
        let mut state = self.state_until(deadline)?;
        Self::validate_transaction(&state, transaction, deadline)?;
        state
            .staging
            .put(&Self::key("MANIFEST")?, manifest)
            .map_err(WorkspaceCheckpoints::error)?;
        if std::time::Instant::now() >= deadline {
            return Err(CheckpointError::deadline());
        }
        // Publication is the transaction's irrevocable point. Once this write
        // begins, its result wins over the deadline so success is never reported
        // as timeout after the new generation became authoritative.
        self.storage
            .put(&self.current_key, state.generation.as_bytes())
            .map_err(WorkspaceCheckpoints::error)?;

        state.current = Some(state.staging.clone());
        state.generation = WorkspaceCheckpoints::generation();
        state.staging = Namespace::new(
            self.storage.clone(),
            self.root.join(&state.generation).map_err(WorkspaceCheckpoints::error)?,
        );
        state.transaction = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    trait TestCheckpoint {
        fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError>;
        fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError>;
        fn abort(&self) -> Result<(), CheckpointError>;
        fn transaction(&self) -> NonZeroU64;
    }

    thread_local! {
        static TRANSACTIONS: RefCell<HashMap<usize, NonZeroU64>> = RefCell::new(HashMap::new());
    }

    impl<T: CheckpointImage + ?Sized> TestCheckpoint for T {
        fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
            let key = std::ptr::from_ref(self).cast::<()>() as usize;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let transaction =
                if let Some(transaction) = TRANSACTIONS.with(|transactions| transactions.borrow().get(&key).copied()) {
                    transaction
                } else {
                    let transaction = self.begin_until(deadline)?;
                    TRANSACTIONS.with(|transactions| transactions.borrow_mut().insert(key, transaction));
                    transaction
                };
            self.put_until(transaction, name, bytes, deadline)
        }

        fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
            let key = std::ptr::from_ref(self).cast::<()>() as usize;
            let transaction = TRANSACTIONS.with(|transactions| transactions.borrow()[&key]);
            let result = self.commit_until(
                transaction,
                manifest,
                std::time::Instant::now() + std::time::Duration::from_secs(10),
            );
            if result.is_ok() {
                TRANSACTIONS.with(|transactions| transactions.borrow_mut().remove(&key));
            }
            result
        }

        fn abort(&self) -> Result<(), CheckpointError> {
            let key = std::ptr::from_ref(self).cast::<()>() as usize;
            let transaction = TRANSACTIONS
                .with(|transactions| transactions.borrow_mut().remove(&key))
                .expect("test transaction");
            self.abort_until(
                transaction,
                std::time::Instant::now() + std::time::Duration::from_secs(10),
            )
        }

        fn transaction(&self) -> NonZeroU64 {
            let key = std::ptr::from_ref(self).cast::<()>() as usize;
            TRANSACTIONS.with(|transactions| transactions.borrow()[&key])
        }
    }

    fn publish_external(workspace: &std::path::Path, namespace: &str, generation: &str, state: &[u8]) {
        let storage = Directory::open(workspace).unwrap();
        let root = Key::parse("checkpoints").unwrap().join(namespace).unwrap();
        let generation_key = root.join(generation).unwrap();
        let published = Namespace::new(storage.clone(), generation_key);
        published.put(&Key::parse("state").unwrap(), state).unwrap();
        published.put(&Key::parse("MANIFEST").unwrap(), b"external").unwrap();
        storage.put(&root.join("current").unwrap(), generation.as_bytes()).unwrap();
    }

    #[test]
    fn reopening_refreshes_a_generation_published_through_another_storage_handle() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let cached = images.open("shared-generation").unwrap();
        cached.put("state", b"first").unwrap();
        cached.commit(b"first-manifest").unwrap();

        publish_external(temporary.path(), "shared-generation", "generation-external", b"second");

        let reopened = images.open("shared-generation").unwrap();
        assert!(Arc::ptr_eq(&cached, &reopened));
        assert_eq!(reopened.get("state").unwrap(), b"second");
        assert_eq!(reopened.get("MANIFEST").unwrap(), b"external");
    }

    #[test]
    fn a_delayed_refresh_cannot_roll_the_cached_generation_back() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let namespace = "serialized-refresh";
        let cached = images.open(namespace).unwrap();
        cached.put("state", b"first").unwrap();
        cached.commit(b"first-manifest").unwrap();
        publish_external(temporary.path(), namespace, "generation-refresh-barrier", b"second");

        let key = format!("{}:{namespace}", images.identity);
        let concrete = IMAGES
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .get(&key)
            .unwrap()
            .upgrade()
            .unwrap();
        let state = concrete.state().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        *REFRESH_BEFORE_STATE.get_or_init(|| Mutex::new(None)).lock().unwrap() =
            Some((key, barrier.clone()));
        *WATCHED_CURRENT.get_or_init(|| Mutex::new(None)).lock().unwrap() =
            Some("generation-refresh-barrier".to_owned());
        WATCHED_CURRENT_READS.store(0, Ordering::Relaxed);

        let workspace = temporary.path().to_owned();
        let refresh = std::thread::spawn(move || {
            WorkspaceCheckpoints::open(&workspace)
                .unwrap()
                .open(namespace)
                .unwrap()
        });
        barrier.wait();
        assert_eq!(WATCHED_CURRENT_READS.load(Ordering::Relaxed), 0);
        barrier.wait();
        assert_eq!(WATCHED_CURRENT_READS.load(Ordering::Relaxed), 0);
        drop(state);
        let refreshed = refresh.join().unwrap();

        *REFRESH_BEFORE_STATE.get().unwrap().lock().unwrap() = None;
        *WATCHED_CURRENT.get().unwrap().lock().unwrap() = None;
        assert_eq!(WATCHED_CURRENT_READS.load(Ordering::Relaxed), 1);
        assert_eq!(refreshed.get("state").unwrap(), b"second");
    }

    #[test]
    fn an_active_capture_keeps_its_view_until_the_transaction_ends() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let cached = images.open("active-generation").unwrap();
        cached.put("state", b"first").unwrap();
        cached.commit(b"first-manifest").unwrap();
        cached.put("staging", b"owned").unwrap();

        publish_external(temporary.path(), "active-generation", "generation-external", b"second");

        let active = images.open("active-generation").unwrap();
        assert_eq!(active.get("state").unwrap(), b"first");
        active.abort().unwrap();
        let refreshed = images.open("active-generation").unwrap();
        assert_eq!(refreshed.get("state").unwrap(), b"second");
    }

    #[test]
    fn an_active_capture_ignores_missing_or_malformed_external_current() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let namespace = "active-invalid-current";
        let cached = images.open(namespace).unwrap();
        cached.put("state", b"first").unwrap();
        cached.commit(b"first-manifest").unwrap();
        cached.put("staging", b"owned").unwrap();

        let storage = Directory::open(temporary.path()).unwrap();
        let current = Key::parse("checkpoints")
            .unwrap()
            .join(namespace)
            .unwrap()
            .join("current")
            .unwrap();
        storage.put(&current, &[0xff]).unwrap();
        assert_eq!(images.open(namespace).unwrap().get("state").unwrap(), b"first");
        storage.remove(&current).unwrap();
        assert_eq!(images.open(namespace).unwrap().get("state").unwrap(), b"first");

        cached.abort().unwrap();
        let reopened = images.open(namespace).unwrap();
        assert!(reopened.get("state").is_err());
    }

    #[test]
    fn a_current_pointer_never_selects_an_incomplete_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = Directory::open(temporary.path()).unwrap();
        let root = Key::parse("checkpoints").unwrap().join("incomplete").unwrap();
        let generation = Namespace::new(storage.clone(), root.join("generation-incomplete").unwrap());
        generation.put(&Key::parse("state").unwrap(), b"orphan").unwrap();
        storage
            .put(&root.join("current").unwrap(), b"generation-incomplete")
            .unwrap();

        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        assert!(images.open("incomplete").is_err());
    }

    #[test]
    fn providers_for_one_workspace_share_capture_ownership() {
        let temporary = tempfile::tempdir().unwrap();
        let first_images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let second_images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let first = first_images.open("shared").unwrap();
        let second = second_images.open("shared").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let owner = first.begin_until(deadline).unwrap();
        first.put_until(owner, "first", b"owned", deadline).unwrap();
        assert!(second.begin_until(deadline).is_err());
        first.abort_until(owner, deadline).unwrap();
        assert!(second.begin_until(deadline).is_ok());
    }

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

        assert_eq!(images.open("alpha").unwrap().get("state").unwrap(), b"alpha");
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

    #[test]
    fn abort_preserves_committed_generation_and_clears_retry_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let images = WorkspaceCheckpoints::open(temporary.path()).unwrap();
        let image = images.open("terminal-abort").unwrap();
        image.put("state", b"first").unwrap();
        image.commit(b"manifest-one").unwrap();

        image.put("stale", b"must-not-survive").unwrap();
        assert!(image
            .abort_until(image.transaction(), std::time::Instant::now())
            .unwrap_err()
            .is_deadline());
        image.abort().unwrap();
        assert_eq!(image.get("state").unwrap(), b"first");
        assert_eq!(image.get("MANIFEST").unwrap(), b"manifest-one");

        image.put("state", b"second").unwrap();
        image.commit(b"manifest-two").unwrap();
        assert_eq!(image.get("state").unwrap(), b"second");
        assert!(!image.list().unwrap().iter().any(|name| name == "stale"));
    }
}
