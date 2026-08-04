use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;

use super::registry::{Registration, Registry};
use crate::device::{Error, Host, Id, Node, NodeKind, ObjectToken, OpenCapability, Scope};
use crate::{
    BUILTIN_DEVICES, GuestPathBytes, Kind, MountKind, MountNamespace, MountRoute, MountSourceId, Permissions,
    ProjectedObjectId,
};

struct Fixture;

impl Fixture {
    fn object(value: u64) -> ProjectedObjectId {
        ProjectedObjectId::new(value).expect("nonzero object")
    }

    fn registration(path: &str, object_value: u64) -> Registration {
        Registration {
            path: GuestPathBytes::new(path.as_bytes()).expect("valid path"),
            scope: Scope::Root,
            device: Id::new(226, 128),
            kind: NodeKind::OpaqueCharacter,
            permissions: Permissions::from_bits(0o660),
            user: 12,
            group: 34,
            object: Self::object(object_value),
        }
    }

    fn await_publication(registry: &Registry, published: &mpsc::Receiver<()>) {
        let path = GuestPathBytes::new(b"/dev/race").unwrap();
        let mut observed = None;
        loop {
            if observed.is_none() {
                observed = registry.lookup(&path, MountRoute::Root);
            }
            match published.try_recv() {
                Ok(()) => {
                    let node = Self::completed_node(registry, &path, observed);
                    Self::assert_race_node(&node, &path);
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => thread::yield_now(),
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("publisher disconnected before registration")
                }
            }
        }
    }

    fn completed_node(registry: &Registry, path: &GuestPathBytes, observed: Option<Arc<Node>>) -> Arc<Node> {
        match observed {
            Some(node) => node,
            None => registry
                .lookup(path, MountRoute::Root)
                .expect("completed registration must be visible"),
        }
    }

    fn assert_race_node(node: &Node, path: &GuestPathBytes) {
        assert_eq!(&node.path, path);
        assert_eq!(node.device, Id::new(226, 128));
        assert_eq!(node.object, Self::object(90));
    }
}

#[test]
fn linux_device_width() {
    for device in [
        Id::new(0, 0),
        Id::new(1, 3),
        Id::new(226, 128),
        Id::new(u32::MAX, u32::MAX),
    ] {
        assert_eq!(Id::from_linux_encoded(device.linux_encoded()), device);
    }
}

#[test]
fn builtin_metadata_nodes() {
    let expected = [
        ("/dev/null", 1, 3, 0o666),
        ("/dev/zero", 1, 5, 0o666),
        ("/dev/full", 1, 7, 0o666),
        ("/dev/random", 1, 8, 0o666),
        ("/dev/urandom", 1, 9, 0o666),
        ("/dev/tty", 5, 0, 0o666),
        ("/dev/console", 5, 1, 0o600),
    ];
    for (builtin, (path, major, minor, mode)) in BUILTIN_DEVICES.iter().zip(expected) {
        assert_eq!(builtin.path.as_bytes(), path.as_bytes());
        assert_eq!(builtin.device, Id::new(major, minor));
        assert_eq!(builtin.permissions.bits(), mode);
        assert_eq!(builtin.kind.file_kind(), Kind::Character);
    }
}

#[test]
fn removal_slot_identity() {
    let registry = Registry::new();
    let first = registry.register(Fixture::registration("/dev/render", 1)).unwrap();
    registry.remove(first.id).unwrap();
    let second = registry.register(Fixture::registration("/dev/render", 2)).unwrap();

    assert_eq!(first.id.slot, second.id.slot);
    assert_ne!(first.id.generation, second.id.generation);
    assert_eq!(registry.remove(first.id), Err(Error::Stale));
    assert_eq!(
        registry.lookup(&GuestPathBytes::new(b"/dev/render").unwrap(), MountRoute::Root),
        Some(second)
    );
    assert_eq!(first.object, Fixture::object(1));
}

#[test]
fn mount_route_namespace() {
    let registry = Registry::new();
    let source = MountSourceId::new(77).unwrap();
    let mut mounted = Fixture::registration("/dev/dri/renderD128", 9);
    mounted.scope = Scope::Mounted(source);
    let node = registry.register(mounted).unwrap();
    let mounts = MountNamespace::new();
    mounts
        .mount("/dev/dri/renderD128", source, MountKind::File, false)
        .unwrap();
    let path = GuestPathBytes::new(b"/dev/dri/renderD128").unwrap();

    assert!(registry.lookup(&path, MountRoute::Root).is_none());
    assert_eq!(registry.lookup(&path, mounts.route_bytes(&path)), Some(node));
}

#[test]
fn invalid_utf8_restore() {
    let registry = Registry::new();
    let source = MountSourceId::new(78).unwrap();
    let path = GuestPathBytes::new(b"/dev/dri/\xff").unwrap();
    let mut registration = Fixture::registration("/placeholder", 10);
    registration.path = path.clone();
    registration.scope = Scope::Mounted(source);
    let node = registry.register(registration).unwrap();
    let mounts = MountNamespace::new();
    mounts.mount("/dev/dri", source, MountKind::Directory, false).unwrap();

    assert_eq!(registry.lookup(&path, mounts.route_bytes(&path)), Some(node.clone()),);
    let snapshot = registry.snapshot();
    assert_eq!(snapshot[0].registration.path.as_bytes(), b"/dev/dri/\xff");
    let restored = Registry::restore(&snapshot).unwrap();
    assert_eq!(restored.lookup(&path, mounts.route_bytes(&path)), Some(node),);
}

#[test]
fn removed_handles_alive() {
    let registry = Registry::new();
    let handle = registry.register(Fixture::registration("/dev/projected", 41)).unwrap();
    registry.remove(handle.id).unwrap();

    assert_eq!(handle.object, Fixture::object(41));
    assert_eq!(handle.device, Id::new(226, 128));
    assert_eq!(handle.permissions.bits(), 0o660);
}

#[derive(Default)]
struct FakeHost {
    calls: Mutex<Vec<(ProjectedObjectId, OpenCapability)>>,
}

impl Host for FakeHost {
    type Error = ();

    fn open(&self, object: ProjectedObjectId, capability: OpenCapability) -> Result<ObjectToken, Self::Error> {
        self.calls.lock().unwrap().push((object, capability));
        Ok(ObjectToken(object.get() + 100))
    }
}

#[test]
fn host_port_capability() {
    let host = FakeHost::default();
    assert_eq!(
        host.open(Fixture::object(5), OpenCapability::ReadWrite),
        Ok(ObjectToken(105))
    );
    assert_eq!(
        *host.calls.lock().unwrap(),
        vec![(Fixture::object(5), OpenCapability::ReadWrite)]
    );
}

#[test]
fn checkpoint_values_generation() {
    let registry = Registry::new();
    let node = registry.register(Fixture::registration("/dev/checkpoint", 51)).unwrap();
    let snapshot = registry.snapshot();

    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].id, node.id);
    assert_eq!(snapshot[0].registration.object, Fixture::object(51));
    assert_eq!(snapshot[0].registration.path, node.path);
    let restored = Registry::restore(&snapshot).unwrap();
    let restored_node = restored
        .lookup(&GuestPathBytes::new(b"/dev/checkpoint").unwrap(), MountRoute::Root)
        .unwrap();
    assert_eq!(*restored_node, *node);
    restored.remove(node.id).unwrap();
    let reused = restored.register(Fixture::registration("/dev/checkpoint", 52)).unwrap();
    assert!(reused.id.generation > node.id.generation);
}

#[test]
fn repeated_reuse_unique() {
    let registry = Registry::new();
    let mut prior = None;
    for value in 1..=10_000 {
        let node = registry.register(Fixture::registration("/dev/reused", value)).unwrap();
        if let Some(id) = prior {
            assert!(node.id.generation > id);
        }
        prior = Some(node.id.generation);
        registry.remove(node.id).unwrap();
    }
}

#[test]
fn registry_capacity_slot() {
    let registry = Registry::new();
    let mut first = None;
    for value in 1..=256 {
        let path = format!("/dev/projected{value}");
        let node = registry.register(Fixture::registration(&path, value)).unwrap();
        first.get_or_insert(node.id);
    }
    assert_eq!(
        registry.register(Fixture::registration("/dev/overflow", 257)),
        Err(Error::Capacity)
    );
    registry.remove(first.unwrap()).unwrap();
    assert!(
        registry
            .register(Fixture::registration("/dev/replacement", 258))
            .is_ok()
    );
}

#[test]
fn concurrent_lookup_publications() {
    for _ in 0..1_000 {
        let registry = Arc::new(Registry::new());
        let barrier = Arc::new(Barrier::new(2));
        let (published, observed) = mpsc::channel();
        let reader_registry = Arc::clone(&registry);
        let reader_barrier = Arc::clone(&barrier);
        let reader = thread::spawn(move || {
            reader_barrier.wait();
            Fixture::await_publication(&reader_registry, &observed);
        });
        barrier.wait();
        registry.register(Fixture::registration("/dev/race", 90)).unwrap();
        published.send(()).unwrap();
        reader.join().unwrap();
    }
}
