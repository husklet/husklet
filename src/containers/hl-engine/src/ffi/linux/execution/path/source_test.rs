use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{ObjectError, OpenFileDescription};
use hl_runtime::{GuestPath, MountKind, MountRoute, OpenIntent, PreparedPathOpen, RuntimePathError, VfsHost};

use super::file::NativeFile;
use super::open::PendingOpen;
use super::source::{OrdinaryContext, Source};
use super::tmpfs::{Budget, Key};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TestRoot(PathBuf);

impl TestRoot {
    fn create() -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hl-engine-shm-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&path).expect("create isolated test root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove isolated test root");
    }
}

#[test]
fn posix_mount() {
    let root = TestRoot::create();
    let context = OrdinaryContext::new(root.path().as_os_str().as_bytes()).expect("ordinary namespace");
    let shared = GuestPath::new("/dev/shm/object").expect("guest path");

    let source = match context.mounts().route(&shared) {
        MountRoute::Mounted {
            source,
            kind: MountKind::Directory,
            read_only: false,
            ..
        } => source,
        route => panic!("unexpected shared-memory route: {route:?}"),
    };
    assert_eq!(source.get(), 1);

    let host = context.host();
    let mount = host.pin_mount(source).expect("pin mounted root");
    host.close(mount);
    assert_eq!(
        std::fs::metadata(root.path().join("dev/shm"))
            .expect("shared-memory metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o1777
    );
}

#[test]
fn reverse_projection() {
    let root = TestRoot::create();
    let context = OrdinaryContext::new(root.path().as_os_str().as_bytes()).expect("ordinary namespace");

    assert_eq!(
        context
            .guest_path(&root.path().join("dev/shm/sem.database"))
            .expect("mounted guest path")
            .as_str(),
        "/dev/shm/sem.database"
    );
    assert_eq!(
        context
            .guest_path(&root.path().join("dev/shmx"))
            .expect("root guest path")
            .as_str(),
        "/dev/shmx"
    );
}

#[test]
fn byte_quota() {
    let root = TestRoot::create();
    let path = root.path().join("segment");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .expect("create segment");
    let budget = Budget::testing(4, 2);
    let lease = budget.open(&file).expect("charge segment");

    assert_eq!(lease.truncate(&file, 4), Ok(()));
    assert_eq!(lease.write_at(&file, 4, b"x"), Err(ObjectError::NoSpace));
    assert_eq!(file.metadata().expect("failed-growth metadata").len(), 4);
    assert_eq!(lease.truncate(&file, 2), Ok(()));
    let second = std::fs::File::create(root.path().join("second")).expect("second segment");
    let second_lease = budget.open(&second).expect("charge second segment");
    assert_eq!(second_lease.truncate(&second, 2), Ok(()));
    assert_eq!(lease.write_at(&file, 2, b"xy"), Err(ObjectError::NoSpace));
    assert_eq!(second_lease.truncate(&second, 0), Ok(()));
    assert_eq!(lease.write_at(&file, 2, b"xy"), Ok(2));
    second_lease.close(second.metadata().expect("second metadata").nlink());
    lease.close(file.metadata().expect("segment metadata").nlink());
}

#[test]
fn inode_quota() {
    let root = TestRoot::create();
    let first = std::fs::File::create(root.path().join("first")).expect("first inode");
    let second = std::fs::File::create(root.path().join("second")).expect("second inode");
    let budget = Budget::testing(64, 1);
    let lease = budget.open(&first).expect("charge first inode");

    assert!(matches!(budget.open(&second), Err(ObjectError::NoSpace)));
    lease.close(first.metadata().expect("first metadata").nlink());
}

#[test]
fn unlinked_lifetime() {
    let root = TestRoot::create();
    let first_path = root.path().join("first");
    let first = std::fs::File::create(&first_path).expect("first inode");
    let metadata = first.metadata().expect("first metadata");
    let key = Key::new(metadata.dev(), metadata.ino());
    let budget = Budget::testing(64, 1);
    let lease = budget.open(&first).expect("charge first inode");
    std::fs::remove_file(first_path).expect("unlink first inode");
    budget.unlink(key, true);

    let second = std::fs::File::create(root.path().join("second")).expect("second inode");
    assert!(matches!(budget.open(&second), Err(ObjectError::NoSpace)));
    lease.close(first.metadata().expect("unlinked metadata").nlink());
    let second_lease = budget.open(&second).expect("released inode charge");
    let second_metadata = second.metadata().expect("second metadata");
    let second_key = Key::new(second_metadata.dev(), second_metadata.ino());
    let second_path = root.path().join("second");
    std::fs::remove_file(second_path).expect("unlink second inode");
    budget.unlink(second_key, true);
    let third = std::fs::File::create(root.path().join("third")).expect("third inode");
    assert!(matches!(budget.open(&third), Err(ObjectError::NoSpace)));
    second_lease.close(second.metadata().expect("unlinked second metadata").nlink());
    let third_lease = budget.open(&third).expect("single release");
    third_lease.close(third.metadata().expect("third metadata").nlink());
}

#[test]
fn hardlink_accounting() {
    let root = TestRoot::create();
    let first_path = root.path().join("first");
    let alias_path = root.path().join("alias");
    let renamed_path = root.path().join("renamed");
    let first = std::fs::File::create(&first_path).expect("first inode");
    let budget = Budget::testing(64, 1);
    let first_lease = budget.open(&first).expect("charge first inode");
    std::fs::hard_link(&first_path, &alias_path).expect("link alias");
    std::fs::rename(&alias_path, &renamed_path).expect("rename alias");
    let alias = std::fs::File::open(&renamed_path).expect("open renamed alias");
    let alias_lease = budget.open(&alias).expect("reuse inode charge");

    alias_lease.close(alias.metadata().expect("alias metadata").nlink());
    first_lease.close(first.metadata().expect("first metadata").nlink());
}

#[test]
fn fork_budget() {
    let root = TestRoot::create();
    let source = Source::ordinary(root.path().as_os_str().as_bytes()).expect("ordinary source");
    let child = source.clone();
    let parent = source.native().expect("parent context");
    let child = child.native().expect("child context");

    assert!(std::ptr::eq(parent, child));
    let path = root.path().join("dev/shm/segment");
    let parent_budget = parent.shm_budget(&path).expect("parent budget");
    let child_budget = child.shm_budget(&path).expect("child budget");
    assert!(Arc::ptr_eq(&parent_budget, &child_budget));
}

#[test]
fn bind_mount_publishes_routing_and_procfs_identity() {
    let root = TestRoot::create();
    let backing = root.path().join("backing");
    std::fs::create_dir(&backing).expect("bind backing");
    let context = OrdinaryContext::new(root.path().as_os_str().as_bytes()).expect("ordinary namespace");
    context
        .mount_directory("/mnt", backing.to_str().expect("utf8 test path"), true)
        .expect("publish bind");
    assert!(matches!(
        context
            .mounts()
            .route(&hl_runtime::GuestPath::new("/mnt/file").unwrap()),
        hl_runtime::MountRoute::Mounted { read_only: true, .. }
    ));
    let mounts = hl_runtime::ProcfsMountPort::snapshot(context.mounts());
    assert!(
        mounts
            .iter()
            .any(|mount| mount.active && mount.guest_path.as_str() == "/mnt")
    );
}

#[test]
fn create_rollback() {
    let root = TestRoot::create();
    let shared = root.path().join("dev/shm");
    std::fs::create_dir_all(&shared).expect("create shared directory");
    let target = shared.join("blocked");
    let parent: std::os::fd::OwnedFd = std::fs::File::open(&shared).expect("open shared directory").into();
    let watches = super::watch::Hub::new(root.path().as_os_str().as_bytes()).expect("watch hub");
    let file = NativeFile::new(
        watches,
        target.clone(),
        Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        Arc::new(super::metadata::Registry::default()),
        Some(Budget::testing(64, 0)),
    );
    let mut open = PendingOpen::new(
        file,
        target.clone(),
        OpenIntent::from_bits(OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::EXCLUSIVE),
        0o600,
        root.path().to_owned(),
        Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        Arc::new(hl_runtime::TerminalCatalog::default()),
        parent.into(),
        std::ffi::CString::new("blocked").expect("target name"),
        Arc::new(super::FileTransferRegistry::default()),
    );

    assert_eq!(open.commit(), Err(RuntimePathError::NoSpace));
    assert!(!target.exists());
}

#[test]
fn path_only_symlink_retains_inode_semantics_after_unlink() {
    let root = TestRoot::create();
    let link = root.path().join("link");
    std::os::unix::fs::symlink("target", &link).expect("create link");
    let parent: std::os::fd::OwnedFd = std::fs::File::open(root.path()).expect("open parent").into();
    let watches = super::watch::Hub::new(root.path().as_os_str().as_bytes()).expect("watch hub");
    let file = NativeFile::new(
        watches,
        link.clone(),
        Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        Arc::new(super::metadata::Registry::default()),
        None,
    );
    let mut open = PendingOpen::new(
        Arc::clone(&file),
        link.clone(),
        OpenIntent::from_bits(OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW),
        0,
        root.path().to_owned(),
        Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        Arc::new(hl_runtime::TerminalCatalog::default()),
        parent.into(),
        std::ffi::CString::new("link").unwrap(),
        Arc::new(super::FileTransferRegistry::default()),
    );
    open.commit().expect("open symlink inode");
    let moved = root.path().join("moved");
    std::fs::rename(&link, &moved).expect("rename retained link");
    std::fs::remove_file(&moved).expect("unlink retained link");

    assert_eq!(file.read_link().unwrap(), b"target");
    assert_eq!(OpenFileDescription::metadata(file.as_ref()).unwrap().kind, 10);
    assert_eq!(
        OpenFileDescription::read(file.as_ref(), &mut [0; 1]),
        Err(ObjectError::BadDescriptor)
    );
}
