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
