use super::{CheckpointImage as _, CheckpointImages as _, DirectoryImage, DirectoryImageState, DirectoryImages};
use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU64;

trait TestCheckpoint {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), super::CheckpointError>;
    fn commit(&self, manifest: &[u8]) -> Result<(), super::CheckpointError>;
    fn abort(&self) -> Result<(), super::CheckpointError>;
    fn transaction(&self) -> NonZeroU64;
}

thread_local! {
    static TRANSACTIONS: RefCell<HashMap<usize, NonZeroU64>> = RefCell::new(HashMap::new());
}

impl<T: super::CheckpointImage + ?Sized> TestCheckpoint for T {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), super::CheckpointError> {
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

    fn commit(&self, manifest: &[u8]) -> Result<(), super::CheckpointError> {
        let key = std::ptr::from_ref(self).cast::<()>() as usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let transaction =
            if let Some(transaction) = TRANSACTIONS.with(|transactions| transactions.borrow().get(&key).copied()) {
                transaction
            } else {
                self.begin_until(deadline)?
            };
        let result = self.commit_until(transaction, manifest, deadline);
        if result.is_ok() {
            TRANSACTIONS.with(|transactions| transactions.borrow_mut().remove(&key));
        }
        result
    }

    fn abort(&self) -> Result<(), super::CheckpointError> {
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

#[test]
fn shared_namespace_serializes_capture_ownership() {
    let temporary = tempfile::tempdir().unwrap();
    let first_images = DirectoryImages::open(temporary.path()).unwrap();
    let second_images = DirectoryImages::open(temporary.path()).unwrap();
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
fn expired_owner_is_reclaimed_and_stale_token_is_fenced() {
    let temporary = tempfile::tempdir().unwrap();
    let images = DirectoryImages::open(temporary.path()).unwrap();
    let image = images.open("reclaim").unwrap();
    let lease = std::time::Instant::now() + std::time::Duration::from_millis(2);
    let stale = image.begin_until(lease).unwrap();
    image.put_until(stale, "stale", b"old", lease).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let current = image.begin_until(deadline).unwrap();
    image.put_until(current, "current", b"new", deadline).unwrap();
    assert!(image.put_until(stale, "late", b"bad", deadline).is_err());
    assert!(image.commit_until(stale, b"bad", deadline).is_err());
    assert!(image.abort_until(stale, deadline).is_err());
    image.commit_until(current, b"manifest", deadline).unwrap();
    assert_eq!(image.get("current").unwrap(), b"new");
    assert!(image.get("stale").is_err());
    assert!(image.get("late").is_err());
}

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
fn abort_discards_only_unpublished_generation_and_reuses_image_cleanly() {
    let temporary = tempfile::tempdir().unwrap();
    let images = DirectoryImages::open(temporary.path().join("checkpoints")).unwrap();
    let image = images.open("container").unwrap();
    image.put("state", b"first").unwrap();
    image.commit(b"manifest-one").unwrap();

    image.put("stale", b"must-not-survive").unwrap();
    assert!(
        image
            .abort_until(image.transaction(), std::time::Instant::now())
            .unwrap_err()
            .is_deadline()
    );
    image.abort().unwrap();
    assert_eq!(image.get("state").unwrap(), b"first");
    assert_eq!(image.get("MANIFEST").unwrap(), b"manifest-one");

    image.put("state", b"second").unwrap();
    image.commit(b"manifest-two").unwrap();
    assert_eq!(image.get("state").unwrap(), b"second");
    assert_eq!(image.get("MANIFEST").unwrap(), b"manifest-two");
    assert!(!image.list().unwrap().iter().any(|name| name == "stale"));
}

#[test]
fn active_capture_cannot_be_superseded_by_another_provider() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("checkpoints");
    let first_process = DirectoryImages::open(root.clone()).unwrap();
    let second_process = DirectoryImages::open(root).unwrap();

    let older = first_process.open("container").unwrap();
    let newer = second_process.open("container").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let owner = older.begin_until(deadline).unwrap();
    older.put_until(owner, "state", b"older", deadline).unwrap();
    assert!(newer.begin_until(deadline).is_err());
    older.commit_until(owner, b"older-manifest", deadline).unwrap();

    let restored = first_process.open("container").unwrap();
    assert_eq!(restored.get("state").unwrap(), b"older");
    assert_eq!(restored.get("MANIFEST").unwrap(), b"older-manifest");
}

#[test]
fn reopening_during_capture_does_not_refresh_its_publication_base() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("checkpoints");
    let images = DirectoryImages::open(&root).unwrap();
    let image = images.open("container").unwrap();
    image.put("state", b"first").unwrap();
    image.commit(b"manifest-one").unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let transaction = image.begin_until(deadline).unwrap();
    image.put_until(transaction, "state", b"candidate", deadline).unwrap();

    let namespace = root.join("container");
    let foreign = "generation-foreign";
    std::fs::create_dir(namespace.join(foreign)).unwrap();
    std::fs::write(namespace.join(foreign).join("MANIFEST"), b"foreign").unwrap();
    std::fs::write(namespace.join("current"), foreign.as_bytes()).unwrap();
    let _reopened = images.open("container").unwrap();

    assert!(image.commit_until(transaction, b"candidate", deadline).is_err());
    assert_eq!(std::fs::read(namespace.join("current")).unwrap(), foreign.as_bytes());
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

#[cfg(unix)]
#[test]
fn expired_storage_deadline_does_not_wait_for_generation_lock() {
    use nix::fcntl::{OFlag, open};
    use nix::sys::stat::Mode;
    use std::sync::Mutex;

    let temporary = tempfile::tempdir().unwrap();
    let directory = open(
        temporary.path(),
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let image = DirectoryImage {
        directory,
        state: Mutex::new(DirectoryImageState {
            current: None,
            base: None,
            generation: "generation-deadline-test".into(),
            transaction: None,
        }),
    };
    let held = image.state.lock().unwrap();
    let started = std::time::Instant::now();
    let error = image
        .put_until(NonZeroU64::MIN, "state", b"late", started)
        .expect_err("expired deadline must fail while the state lock is held");
    assert!(error.to_string().contains("deadline exceeded"));
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    drop(held);
}

#[test]
fn expired_commit_deadline_preserves_authoritative_generation() {
    let temporary = tempfile::tempdir().unwrap();
    let images = DirectoryImages::open(temporary.path()).unwrap();
    let first = images.open("container").unwrap();
    first.put("state", b"first").unwrap();
    first.commit(b"manifest-one").unwrap();

    let candidate = images.open("container").unwrap();
    candidate.put("state", b"second").unwrap();
    let expired = std::time::Instant::now();
    let error = candidate
        .commit_until(candidate.transaction(), b"manifest-two", expired)
        .expect_err("expired capture must not publish");
    assert!(error.to_string().contains("deadline exceeded"));

    let restored = images.open("container").unwrap();
    assert_eq!(restored.get("state").unwrap(), b"first");
    assert_eq!(restored.get("MANIFEST").unwrap(), b"manifest-one");
}

#[test]
fn expired_list_on_empty_image_is_not_reported_as_success() {
    let temporary = tempfile::tempdir().unwrap();
    let images = DirectoryImages::open(temporary.path()).unwrap();
    let empty = images.open("container").unwrap();
    let error = empty
        .list_until(std::time::Instant::now())
        .expect_err("empty storage must still observe capture expiry");
    assert!(error.is_deadline());
}

#[test]
fn publication_lock_deadline_preserves_authoritative_generation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("checkpoints");
    let images = DirectoryImages::open(&root).unwrap();
    let first = images.open("container").unwrap();
    first.put("state", b"first").unwrap();
    first.commit(b"manifest-one").unwrap();

    let candidate = images.open("container").unwrap();
    candidate.put("state", b"second").unwrap();
    let publication_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("container/.publication.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&publication_lock).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(10);
    let error = candidate
        .commit_until(candidate.transaction(), b"manifest-two", deadline)
        .expect_err("publication lock contention must observe the deadline");
    assert!(error.to_string().contains("deadline exceeded"));
    fs2::FileExt::unlock(&publication_lock).unwrap();

    let restored = images.open("container").unwrap();
    assert_eq!(restored.get("state").unwrap(), b"first");
    assert_eq!(restored.get("MANIFEST").unwrap(), b"manifest-one");
}

#[cfg(unix)]
#[test]
fn directory_sync_failure_reports_that_rename_already_published() {
    let outcome = DirectoryImage::publication_after_rename(Err(nix::errno::Errno::EIO));
    let super::storage::PublicationOutcome::PublishedNotDurable(error) = outcome else {
        panic!("a post-rename sync failure must retain the publication outcome");
    };
    assert!(error.publication_occurred());
    assert!(error.to_string().contains("published"));
}

#[cfg(unix)]
#[test]
fn post_rename_sync_failure_advances_in_memory_authority() {
    let mut state = DirectoryImageState {
        current: None,
        base: None,
        generation: "generation-published".into(),
        transaction: None,
    };
    let error = DirectoryImage::finish_publication(
        &mut state,
        b"generation-published".to_vec(),
        super::storage::PublicationOutcome::PublishedNotDurable(crate::CheckpointError::published(
            "injected directory sync failure",
        )),
    )
    .expect_err("durability failure remains observable");

    assert!(error.publication_occurred());
    assert!(matches!(
        state.current,
        Some(super::DirectoryGeneration::Named(ref generation)) if generation == "generation-published"
    ));
    assert_eq!(state.base.as_deref(), Some(b"generation-published".as_slice()));
    assert_ne!(state.generation, "generation-published");
}
