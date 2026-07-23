use super::support::tar_file;
use hl_images::{Images, Platform, Reference, RuntimeConfig};
use std::collections::BTreeMap;

#[test]
#[ignore = "subprocess entry point for the cross-process live-root GC regression"]
fn root_lease_holder_process() {
    let Some(root) = std::env::var_os("HL_ROOT_HOLDER_STORE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let images = Images::open(&root).unwrap();
    let name: Reference = "example.test/live-root:v1".parse().unwrap();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/usr/bin/python".into()],
        command: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let image = images
        .import(
            std::io::Cursor::new(tar_file("usr/bin/python", b"held guest executable")),
            &runtime,
            &Platform::linux_arm64(),
            &name,
        )
        .unwrap();
    let unpacked = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    let reference = images.rootfs(&unpacked).unwrap();
    std::fs::write(root.join("holder-ready"), reference.snapshot().as_str()).unwrap();
    while !root.join("collect-finished").exists() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        images.roots().open(&reference).unwrap().path().is_dir(),
        "GC removed a committed root owned by another process"
    );
    images.roots().release(&reference).unwrap();
}

#[test]
fn cross_process_gc_preserves_a_live_committed_container_root() {
    let temporary = tempfile::tempdir().unwrap();
    // Open before the holder publishes either its image or lease. This is the
    // stale-reader ordering that parallel scenario category processes exercise.
    let collector = Images::open(temporary.path()).unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut holder = std::process::Command::new(executable)
        .args([
            "--ignored",
            "--exact",
            "suite::lease::root_lease_holder_process",
        ])
        .env("HL_ROOT_HOLDER_STORE", temporary.path())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !temporary.path().join("holder-ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "root holder did not publish its lease"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let snapshot = std::fs::read_to_string(temporary.path().join("holder-ready")).unwrap();
    collector.gc().unwrap();
    assert!(
        temporary
            .path()
            .join("snapshots/committed")
            .join(snapshot)
            .is_dir(),
        "collector removed the other process's live committed root"
    );
    std::fs::write(temporary.path().join("collect-finished"), b"").unwrap();
    assert!(holder.wait().unwrap().success());
}

#[test]
fn unpacked_image_lease_bridges_unpack_to_rootfs_across_gc() {
    let temporary = tempfile::tempdir().unwrap();
    let images = Images::open(temporary.path()).unwrap();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/usr/bin/python".into()],
        command: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let image = images
        .import(
            std::io::Cursor::new(tar_file("usr/bin/python", b"guest executable")),
            &runtime,
            &Platform::linux_arm64(),
            &"example.test/unpack-lease:v1".parse().unwrap(),
        )
        .unwrap();
    let unpacked = images.unpack(&image, &Platform::linux_arm64()).unwrap();

    let report = images.gc().unwrap();
    assert!(report.snapshots_kept > 0);
    let root = images.rootfs(&unpacked).unwrap();
    assert_eq!(
        std::fs::read(
            images
                .roots()
                .open(&root)
                .unwrap()
                .path()
                .join("usr/bin/python"),
        )
        .unwrap(),
        b"guest executable"
    );
    images.roots().release(&root).unwrap();
}

#[test]
fn unpack_rebuilds_a_legacy_empty_non_scratch_chain() {
    let temporary = tempfile::tempdir().unwrap();
    let images = Images::open(temporary.path()).unwrap();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/usr/bin/python".into()],
        command: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let image = images
        .import(
            std::io::Cursor::new(tar_file("usr/bin/python", b"restored executable")),
            &runtime,
            &Platform::linux_arm64(),
            &"example.test/empty-chain:v1".parse().unwrap(),
        )
        .unwrap();
    let first = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    let snapshot = first.snapshot().as_str().to_owned();
    drop(first);
    let path = temporary.path().join("snapshots/committed").join(&snapshot);
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::create_dir(&path).unwrap();

    let repaired = images.unpack(&image, &Platform::linux_arm64()).unwrap();
    let root = images.rootfs(&repaired).unwrap();
    assert_eq!(
        std::fs::read(
            images
                .roots()
                .open(&root)
                .unwrap()
                .path()
                .join("usr/bin/python"),
        )
        .unwrap(),
        b"restored executable"
    );
    images.roots().release(&root).unwrap();
}
