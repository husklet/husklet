use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription, Readiness, ReadinessObserver,
    ReadinessRegistry, ReadinessSubscription, StatusFlags,
};

use crate::{GuestPathBytes, Kind, VfsDirectoryDescription, VfsDirectoryEntry, VfsDirectoryHost, VfsFileToken};

#[derive(Clone)]
struct FakeDirectoryHost {
    entries: Arc<Mutex<Vec<VfsDirectoryEntry>>>,
    snapshots: Arc<AtomicUsize>,
    cancels: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    registry: ReadinessRegistry,
}

#[test]
fn stale_batch_snapshot() {
    let host = FakeDirectoryHost::new(&["a", "b"]);
    let description = host.description();
    assert_eq!(description.path().as_bytes(), b"/tmp/\xff");
    let stale = description.read_directory(2).unwrap();
    description.refresh().unwrap();
    assert_eq!(
        description.commit_directory(stale.token, 1),
        Err(ObjectError::InvalidArgument),
    );
    let fresh = description.read_directory(1).unwrap();
    assert_eq!(fresh.entries[0].name, b"a");
}

#[test]
fn duplicate_leases_cursor() {
    let host = FakeDirectoryHost::new(&["a", "b"]);
    let description = host.description();
    let table = DescriptorTable::new(8).unwrap();
    let fd = table.install(0, description, DescriptorFlags::default()).unwrap();
    let duplicate = table.duplicate(fd, 0, DescriptorFlags::default()).unwrap();
    let batch = table.pin(fd).unwrap().read_directory(1).unwrap();
    table.pin(duplicate).unwrap().commit_directory(batch.token, 1).unwrap();
    let next = table.pin(fd).unwrap().read_directory(1).unwrap();
    assert_eq!(next.entries[0].name, b"b");
}

impl FakeDirectoryHost {
    fn new(names: &[&str]) -> Self {
        Self {
            entries: Arc::new(Mutex::new(
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| VfsDirectoryEntry::new(index as u64 + 1, Kind::Regular, *name).unwrap())
                    .collect(),
            )),
            snapshots: Arc::new(AtomicUsize::new(0)),
            cancels: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
            registry: ReadinessRegistry::new(),
        }
    }

    fn description(&self) -> Arc<VfsDirectoryDescription<Self>> {
        Arc::new(VfsDirectoryDescription::new(
            self.clone(),
            VfsFileToken::from_raw(4),
            GuestPathBytes::new(b"/tmp/\xff").unwrap(),
            StatusFlags::default(),
        ))
    }
}

impl VfsDirectoryHost for FakeDirectoryHost {
    fn snapshot(&self, _directory: VfsFileToken) -> Result<Vec<VfsDirectoryEntry>, ObjectError> {
        self.snapshots.fetch_add(1, Ordering::AcqRel);
        Ok(self.entries.lock().unwrap().clone())
    }

    fn readiness(&self, _directory: VfsFileToken, interests: Readiness) -> Readiness {
        interests
    }

    fn subscribe(
        &self,
        _directory: VfsFileToken,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.registry.subscribe(observer)
    }

    fn cancel(&self, _directory: VfsFileToken) {
        self.cancels.fetch_add(1, Ordering::AcqRel);
        self.registry.close();
    }

    fn close(&self, _directory: VfsFileToken) {
        self.closes.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn duplicate_fork_cookie() {
    let host = FakeDirectoryHost::new(&["a", "b", "c"]);
    let description = host.description();
    let table = DescriptorTable::new(8).unwrap();
    let fd = table
        .install(0, description.clone(), DescriptorFlags::default())
        .unwrap();
    let duplicate = table.duplicate(fd, 0, DescriptorFlags::default()).unwrap();
    let fork = table.fork();
    assert_eq!(description.read_entries(1).unwrap()[0].name, b"a".to_vec());
    assert_eq!(
        table.pin(duplicate).unwrap().object().kind(),
        hl_descriptor::ObjectKind::Directory
    );
    assert_eq!(description.read_entries(1).unwrap()[0].name, b"b".to_vec());
    assert_eq!(
        fork.pin(fd).unwrap().object().kind(),
        hl_descriptor::ObjectKind::Directory
    );
    assert_eq!(description.cookie(), 2);
    assert_eq!(host.snapshots.load(Ordering::Acquire), 1);
}

#[test]
fn snapshot_hides_refresh() {
    let host = FakeDirectoryHost::new(&["before"]);
    let description = host.description();
    assert_eq!(description.read_entries(1).unwrap()[0].name, b"before".to_vec());
    host.entries
        .lock()
        .unwrap()
        .push(VfsDirectoryEntry::new(2, Kind::Regular, "after").unwrap());
    assert!(description.read_entries(4).unwrap().is_empty());
    description.refresh().unwrap();
    let names = description
        .read_entries(4)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec![b"before".to_vec(), b"after".to_vec()]);
    assert_eq!(host.snapshots.load(Ordering::Acquire), 2);
}

#[test]
fn cookies_rewind_entry() {
    let host = FakeDirectoryHost::new(&["a", "b", "c"]);
    let description = host.description();
    assert_eq!(description.read_entries(0), Err(ObjectError::InvalidArgument));
    assert_eq!(description.cookie(), 0);
    assert_eq!(description.read_entries(2).unwrap().len(), 2);
    assert_eq!(description.seek_cookie(1), Ok(1));
    assert_eq!(description.read_entries(1).unwrap()[0].name, b"b".to_vec());
    assert_eq!(description.seek_cookie(99), Ok(0));
    assert_eq!(description.read_entries(1).unwrap()[0].name, b"a".to_vec());
}

#[test]
fn reused_descriptor_once() {
    let host = FakeDirectoryHost::new(&["old"]);
    let table = DescriptorTable::new(1).unwrap();
    let first = host.description();
    let fd = table.install(0, first.clone(), DescriptorFlags::default()).unwrap();
    first.read_entries(1).unwrap();
    table.close(fd).unwrap();
    drop(first);
    host.entries.lock().unwrap()[0].name = b"new".to_vec();
    let second = host.description();
    let reused = table.install(0, second.clone(), DescriptorFlags::default()).unwrap();
    assert_eq!(reused, fd);
    assert_eq!(second.read_entries(1).unwrap()[0].name, b"new".to_vec());
    table.close(reused).unwrap();
    drop(second);
    assert_eq!(host.cancels.load(Ordering::Acquire), 2);
    assert_eq!(host.closes.load(Ordering::Acquire), 2);
}
