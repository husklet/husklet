use std::collections::BTreeMap;

use hl_images::{LeaseStore, Leases};

#[test]
#[ignore = "subprocess entry point for the cross-process lease regression"]
fn lease_writer_process() {
    let Some(root) = std::env::var_os("HL_LEASE_WRITER_ROOT") else {
        return;
    };
    let writer = std::env::var("HL_LEASE_WRITER_ID").unwrap();
    let manager = Leases::open(&root).unwrap();
    std::fs::write(
        std::path::Path::new(&root).join(format!("ready-{writer}")),
        b"",
    )
    .unwrap();
    while !std::path::Path::new(&root).join("go").exists() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    manager
        .create_with(BTreeMap::new(), [format!("snapshot:root-{writer}")])
        .unwrap();
}

#[test]
fn a_lease_only_releases_resources_it_owns() {
    let temp = tempfile::tempdir().unwrap();
    let manager = Leases::open(temp.path()).unwrap();
    let lease = manager
        .create(BTreeMap::from([("purpose".into(), "pull".into())]))
        .unwrap();
    manager.add(lease.id(), "content:sha256:abc").unwrap();
    let persisted = manager.get(lease.id()).unwrap().unwrap();
    assert!(persisted.owns("content:sha256:abc"));
    assert!(manager.remove(lease.id(), "content:sha256:other").is_err());
    assert!(manager
        .get(lease.id())
        .unwrap()
        .unwrap()
        .owns("content:sha256:abc"));
    manager.remove(lease.id(), "content:sha256:abc").unwrap();
    assert!(!manager
        .get(lease.id())
        .unwrap()
        .unwrap()
        .owns("content:sha256:abc"));
}

#[test]
fn initial_resources_are_owned_when_the_lease_becomes_visible() {
    let temp = tempfile::tempdir().unwrap();
    let manager = Leases::open(temp.path()).unwrap();
    let lease = manager
        .create_with(BTreeMap::new(), ["snapshot:root".into()])
        .unwrap();

    assert!(lease.owns("snapshot:root"));
    assert!(manager
        .get(lease.id())
        .unwrap()
        .unwrap()
        .owns("snapshot:root"));
}

#[test]
fn independently_opened_lease_stores_preserve_each_others_roots() {
    let temp = tempfile::tempdir().unwrap();
    let first = Leases::open(temp.path()).unwrap();
    let second = Leases::open(temp.path()).unwrap();

    let first_lease = first.create(BTreeMap::new()).unwrap();
    first.add(first_lease.id(), "snapshot:first-root").unwrap();
    let second_lease = second.create(BTreeMap::new()).unwrap();
    second
        .add(second_lease.id(), "snapshot:second-root")
        .unwrap();

    let reopened = Leases::open(temp.path()).unwrap();
    let leases = reopened.list().unwrap();
    assert_eq!(leases.len(), 2);
    assert!(leases.iter().any(|lease| lease.owns("snapshot:first-root")));
    assert!(leases
        .iter()
        .any(|lease| lease.owns("snapshot:second-root")));
}

#[test]
fn concurrent_processes_preserve_every_snapshot_lease() {
    const WRITERS: usize = 8;
    let temp = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut children = (0..WRITERS)
        .map(|writer| {
            std::process::Command::new(&executable)
                .args(["--ignored", "--exact", "lease_writer_process"])
                .env("HL_LEASE_WRITER_ROOT", temp.path())
                .env("HL_LEASE_WRITER_ID", writer.to_string())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (0..WRITERS).any(|writer| !temp.path().join(format!("ready-{writer}")).exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "writers did not become ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::fs::write(temp.path().join("go"), b"").unwrap();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }

    let leases = Leases::open(temp.path()).unwrap().list().unwrap();
    assert_eq!(leases.len(), WRITERS);
    for writer in 0..WRITERS {
        assert!(leases
            .iter()
            .any(|lease| lease.owns(&format!("snapshot:root-{writer}"))));
    }
}
