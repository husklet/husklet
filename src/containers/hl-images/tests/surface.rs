use hl_images::{
    LeaseStore, Leases, Platform, Reference,
    remote::{Auth, Registry},
    rootfs::Roots,
    snapshot::{Id, Snapshots},
};

#[test]
fn registry_transport_security_is_explicit() {
    let _secure = Registry::new(Auth::Anonymous);
    let _explicitly_insecure = Registry::insecure(Auth::Anonymous);
}

#[test]
fn public_snapshot_and_platform_surface_is_composable() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let active = snapshots.prepare(Id::new("layer-1").unwrap(), None).unwrap();
    std::fs::write(active.path().join("hello"), "world").unwrap();
    let committed = active.commit(Id::new("sha256-chain").unwrap()).unwrap();
    assert_eq!(
        std::fs::read_to_string(committed.path().join("hello")).unwrap(),
        "world"
    );
    assert_eq!(snapshots.view(committed.id()).unwrap().path(), committed.path());
    let leases = Leases::open(temp.path().join("leases")).unwrap();
    let roots = Roots::new(snapshots.clone(), leases.clone());
    let reference = roots.pin(committed.id()).unwrap();
    let lease_id = reference.lease_id().to_owned();
    let serialized = serde_json::to_vec(&reference).unwrap();
    drop(reference);
    assert!(leases.get(&lease_id).unwrap().is_some());
    let reference = serde_json::from_slice(&serialized).unwrap();
    let reopened = roots.open(&reference).unwrap();
    assert_eq!(std::fs::read_to_string(reopened.path().join("hello")).unwrap(), "world");
    drop(reopened);
    assert!(leases.get(&lease_id).unwrap().is_some());
    roots.release(&reference).unwrap();
    assert!(leases.get(&lease_id).unwrap().is_none());
    assert_eq!(Platform::linux_arm64().architecture, "arm64");
    assert!("alpine".parse::<Reference>().is_ok());
}

#[cfg(unix)]
#[test]
fn snapshot_fork_preserves_unreadable_files() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let parent = snapshots.prepare(Id::new("parent").unwrap(), None).unwrap();
    let secret = parent.path().join("secret");
    std::fs::write(&secret, "value").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    let parent = parent.commit(Id::new("parent-committed").unwrap()).unwrap();

    let child = snapshots.prepare(Id::new("child").unwrap(), Some(parent.id())).unwrap();
    assert_eq!(
        std::fs::metadata(child.path().join("secret"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0
    );
    assert_eq!(
        std::fs::metadata(parent.path().join("secret"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0
    );
}

#[cfg(unix)]
#[test]
fn snapshot_removal_handles_readonly_directories() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let draft = snapshots.prepare(Id::new("readonly").unwrap(), None).unwrap();
    let directory = draft.path().join("locked");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("value"), "value").unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    let snapshot = draft.commit(Id::new("readonly-committed").unwrap()).unwrap();
    let id = snapshot.id().clone();
    drop(snapshot);

    assert!(snapshots.remove(&id).unwrap());
    assert!(snapshots.view(&id).is_err());
}
