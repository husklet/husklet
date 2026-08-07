use crate::{
    GuestPath, GuestPathBytes, MountError, MountKind, MountNamespace, MountRoute, MountSourceId, OpenDecision,
    OpenDirectory, OpenIntent, OpenPlan, OpenRequest, OverlayAction, PathError, ReadOnlyError, ReadOnlyPaths,
    SyntheticFilesystem,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

struct OpenFixture;

impl OpenFixture {
    fn request(path: &str, bits: u32) -> OpenRequest {
        OpenRequest {
            guest_path: GuestPathBytes::new(path.as_bytes()).unwrap(),
            directory: OpenDirectory::from_raw(41),
            intent: OpenIntent::from_bits(bits),
            overlay: false,
            read_only: false,
            final_symlink: false,
        }
    }
}

struct PublicationFixture;

// The fixtures take owned handles because each is moved into a spawned thread.
#[allow(clippy::needless_pass_by_value)]
impl PublicationFixture {
    fn read_only_reader(
        paths: Arc<ReadOnlyPaths>,
        start: Arc<Barrier>,
        finished: Arc<AtomicBool>,
        target: GuestPath,
    ) -> bool {
        start.wait();
        while !finished.load(Ordering::Acquire) {
            let _ = paths.denies(&target);
        }
        paths.denies(&target)
    }

    fn mount_reader(
        namespace: Arc<MountNamespace>,
        start: Arc<Barrier>,
        finished: Arc<AtomicBool>,
        path: GuestPath,
    ) -> MountRoute {
        start.wait();
        while !finished.load(Ordering::Acquire) {
            Self::assert_published_route(namespace.route(&path));
        }
        namespace.route(&path)
    }

    fn assert_published_route(route: MountRoute) {
        let MountRoute::Mounted {
            source,
            kind,
            read_only,
            ..
        } = route
        else {
            return;
        };
        assert_eq!(source, MountSourceId::new(77).unwrap());
        assert_eq!(kind, MountKind::Directory);
        assert!(read_only);
    }
}

#[test]
fn absolute_paths_plan() {
    let path = GuestPath::new("//../../a/./b/../c//").unwrap();
    assert_eq!(path.as_str(), "/a/c");
    assert_eq!(GuestPath::new("/../../").unwrap().as_str(), "/");
    assert_eq!(GuestPath::new("a/../b").unwrap().as_str(), "a/../b");
}

#[test]
fn path_bounds_inputs() {
    assert_eq!(GuestPath::new(""), Err(PathError::Empty));
    assert_eq!(GuestPath::new(&"x".repeat(4_097)), Err(PathError::TooLong));
    let flood = format!("/{}", vec!["x"; 513].join("/"));
    assert_eq!(GuestPath::new(&flood), Err(PathError::TooManyComponents));
}

#[test]
fn subtree_comparison_boundaries() {
    let root = GuestPath::new("/srv/data").unwrap();
    assert!(GuestPath::new("/srv/data").unwrap().is_within(&root));
    assert!(GuestPath::new("/srv/data/index").unwrap().is_within(&root));
    assert!(!GuestPath::new("/srv/database").unwrap().is_within(&root));
}

#[test]
fn read_paths_contract() {
    let paths = ReadOnlyPaths::new();
    assert!(paths.is_empty());
    assert_eq!(paths.add("relative"), Err(ReadOnlyError::RelativePath));
    paths.add("/srv/data").unwrap();
    paths.add("/srv/data").unwrap();
    assert_eq!(paths.len(), 1);
    assert!(paths.denies(&GuestPath::new("/srv/data/index").unwrap()));
    assert!(!paths.denies(&GuestPath::new("/srv/database").unwrap()));
    for index in 1..16 {
        paths.add(&format!("/entry/{index}")).unwrap();
    }
    assert_eq!(paths.add("/overflow"), Err(ReadOnlyError::Capacity));
    assert_eq!(
        ReadOnlyPaths::new().add(&format!("/{}", "x".repeat(255))),
        Err(ReadOnlyError::PathTooLong)
    );
}

#[test]
fn read_entries_semantics() {
    let paths = ReadOnlyPaths::new();
    paths.add("/srv/../secret").unwrap();
    paths.add("/trailing/").unwrap();
    paths.add("/").unwrap();

    assert!(!paths.denies(&GuestPath::new("/secret").unwrap()));
    assert!(!paths.denies(&GuestPath::new("/trailing").unwrap()));
    assert!(!paths.denies(&GuestPath::new("/ordinary").unwrap()));
}

#[test]
fn read_append_readers() {
    const READERS: usize = 8;
    let paths = Arc::new(ReadOnlyPaths::new());
    let start = Arc::new(Barrier::new(READERS + 1));
    let finished = Arc::new(AtomicBool::new(false));
    let target = GuestPath::new("/published/child").unwrap();
    let mut readers = Vec::new();

    for _ in 0..READERS {
        let paths = paths.clone();
        let start = start.clone();
        let finished = finished.clone();
        let target = target.clone();
        readers.push(thread::spawn(move || {
            PublicationFixture::read_only_reader(paths, start, finished, target)
        }));
    }

    start.wait();
    paths.add("/published").unwrap();
    finished.store(true, Ordering::Release);
    for reader in readers {
        assert!(reader.join().unwrap());
    }
}

#[test]
fn open_plan_access() {
    for (path, filesystem) in [
        ("/proc/self/status", SyntheticFilesystem::Proc),
        ("/sys/devices", SyntheticFilesystem::Sys),
        ("/dev/null", SyntheticFilesystem::Dev),
    ] {
        let plan = OpenPlan::build(OpenFixture::request(path, OpenIntent::READ)).unwrap();
        assert_eq!(plan.decision(), OpenDecision::Synthetic(filesystem));
    }
}

#[test]
fn open_plan_precedence() {
    let mut request = OpenFixture::request("/etc/config", OpenIntent::READ);
    request.overlay = true;
    assert_eq!(
        OpenPlan::build(request.clone()).unwrap().decision(),
        OpenDecision::Overlay(OverlayAction::Lookup)
    );
    request.intent = OpenIntent::from_bits(OpenIntent::WRITE);
    assert_eq!(
        OpenPlan::build(request.clone()).unwrap().decision(),
        OpenDecision::Overlay(OverlayAction::CopyUp)
    );
    request.intent = OpenIntent::from_bits(OpenIntent::WRITE | OpenIntent::CREATE);
    assert_eq!(
        OpenPlan::build(request.clone()).unwrap().decision(),
        OpenDecision::Overlay(OverlayAction::Create)
    );
}

#[test]
fn read_temporary_ordering() {
    let mut request = OpenFixture::request("/tmp", OpenIntent::TEMPORARY);
    request.read_only = true;
    assert_eq!(
        OpenPlan::build(request.clone()).unwrap().decision(),
        OpenDecision::PermissionDenied
    );
    request.read_only = false;
    assert_eq!(OpenPlan::build(request).unwrap().decision(), OpenDecision::Temporary);
}

#[test]
fn final_symlink_nofollow() {
    let mut request = OpenFixture::request("/link", OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW);
    request.final_symlink = true;
    assert!(OpenPlan::build(request.clone()).unwrap().names_symlink());
    request.intent = OpenIntent::from_bits(OpenIntent::NOFOLLOW);
    assert!(!OpenPlan::build(request).unwrap().names_symlink());
}

#[test]
fn open_plan_fields() {
    let mut request = OpenFixture::request("/proc/link", OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW);
    request.final_symlink = true;
    let plan = OpenPlan::build(request.clone()).unwrap();
    assert_eq!(plan.directory(), OpenDirectory::from_raw(41));
    assert!(!plan.names_symlink());

    request.guest_path = GuestPathBytes::new(b"/read-only/link").unwrap();
    request.read_only = true;
    request.intent = OpenIntent::from_bits(OpenIntent::WRITE | OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW);
    assert!(!OpenPlan::build(request).unwrap().names_symlink());
}

#[test]
fn unknown_intent_value() {
    let intent = OpenIntent::from_bits(1 << 31);
    let plan = OpenPlan::build(OpenRequest {
        guest_path: GuestPathBytes::new(b"/ordinary").unwrap(),
        directory: OpenDirectory::from_raw(41),
        intent,
        overlay: false,
        read_only: false,
        final_symlink: false,
    })
    .unwrap();
    assert_eq!(plan.intent().bits(), 1 << 31);
    assert_eq!(plan.decision(), OpenDecision::HostPath);
}

#[test]
fn deepest_mount_order() {
    let namespace = MountNamespace::new();
    let inner = namespace
        .mount("/x/y/z", MountSourceId::new(2).unwrap(), MountKind::Directory, false)
        .unwrap();
    namespace
        .mount("/x/y", MountSourceId::new(1).unwrap(), MountKind::Directory, true)
        .unwrap();
    assert_eq!(
        namespace.route(&GuestPath::new("/x/y/z/file").unwrap()),
        MountRoute::Mounted {
            id: inner,
            source: MountSourceId::new(2).unwrap(),
            kind: MountKind::Directory,
            read_only: false,
        }
    );
}

#[test]
fn equal_mount_jail() {
    let namespace = MountNamespace::new();
    let first = namespace
        .mount("/same", MountSourceId::new(1).unwrap(), MountKind::Directory, true)
        .unwrap();
    namespace
        .mount("/same", MountSourceId::new(2).unwrap(), MountKind::Directory, false)
        .unwrap();

    assert_eq!(
        namespace.route(&GuestPath::new("/same/child").unwrap()),
        MountRoute::Mounted {
            id: first,
            source: MountSourceId::new(1).unwrap(),
            kind: MountKind::Directory,
            read_only: true,
        }
    );
}

#[test]
fn mount_paths_capacity() {
    let namespace = MountNamespace::new();
    assert_eq!(
        namespace.mount(
            &format!("/{}", "x".repeat(255)),
            MountSourceId::new(1).unwrap(),
            MountKind::Directory,
            false,
        ),
        Err(MountError::PathTooLong)
    );
}

#[test]
fn mount_append_route() {
    const READERS: usize = 8;
    let namespace = Arc::new(MountNamespace::new());
    let start = Arc::new(Barrier::new(READERS + 1));
    let finished = Arc::new(AtomicBool::new(false));
    let path = GuestPath::new("/published/child").unwrap();
    let mut readers = Vec::new();

    for _ in 0..READERS {
        let namespace = namespace.clone();
        let start = start.clone();
        let finished = finished.clone();
        let path = path.clone();
        readers.push(thread::spawn(move || {
            PublicationFixture::mount_reader(namespace, start, finished, path)
        }));
    }

    start.wait();
    let id = namespace
        .mount(
            "/published",
            MountSourceId::new(77).unwrap(),
            MountKind::Directory,
            true,
        )
        .unwrap();
    finished.store(true, Ordering::Release);
    for reader in readers {
        assert_eq!(
            reader.join().unwrap(),
            MountRoute::Mounted {
                id,
                source: MountSourceId::new(77).unwrap(),
                kind: MountKind::Directory,
                read_only: true,
            }
        );
    }
}

#[test]
fn file_mounts_suffixes() {
    let namespace = MountNamespace::new();
    namespace
        .mount("/socket", MountSourceId::new(1).unwrap(), MountKind::File, false)
        .unwrap();
    assert!(matches!(
        namespace.route(&GuestPath::new("/socket").unwrap()),
        MountRoute::Mounted { .. }
    ));
    assert_eq!(
        namespace.route(&GuestPath::new("/socket/child").unwrap()),
        MountRoute::Root
    );
    namespace
        .mount(
            "/link",
            MountSourceId::new(2).unwrap(),
            MountKind::ProjectedSymlink,
            true,
        )
        .unwrap();
    assert!(matches!(
        namespace.route(&GuestPath::new("/link/child").unwrap()),
        MountRoute::Mounted { .. }
    ));
}

#[test]
fn unmount_append_route() {
    let namespace = MountNamespace::new();
    let path = GuestPath::new("/data").unwrap();
    namespace
        .mount("/data", MountSourceId::new(1).unwrap(), MountKind::Directory, false)
        .unwrap();
    namespace.unmount(&path).unwrap();
    assert_eq!(namespace.route(&path), MountRoute::Root);
    assert_eq!(namespace.unmount(&path), Err(MountError::NotMounted));
    assert!(!namespace.snapshot()[0].active);
}

#[test]
fn deepest_mount_mounts() {
    let namespace = MountNamespace::new();
    namespace
        .mount("/outer", MountSourceId::new(1).unwrap(), MountKind::Directory, true)
        .unwrap();
    namespace
        .mount("/outer/rw", MountSourceId::new(2).unwrap(), MountKind::Directory, false)
        .unwrap();
    namespace
        .mount(
            "/outer/rw/locked",
            MountSourceId::new(3).unwrap(),
            MountKind::Directory,
            true,
        )
        .unwrap();
    let subtrees = ReadOnlyPaths::new();
    assert!(namespace.denies_write(&GuestPath::new("/outer/file").unwrap(), false, &subtrees));
    assert!(!namespace.denies_write(&GuestPath::new("/outer/rw/file").unwrap(), true, &subtrees));
    assert!(namespace.denies_write(&GuestPath::new("/outer/rw/locked/file").unwrap(), false, &subtrees));
    subtrees.add("/outer/rw").unwrap();
    assert!(!namespace.denies_write(&GuestPath::new("/outer/rw/file").unwrap(), true, &subtrees));
    assert!(namespace.denies_write(&GuestPath::new("/etc/config").unwrap(), true, &subtrees));
    assert!(!namespace.denies_write(&GuestPath::new("/tmp/file").unwrap(), true, &subtrees));
    subtrees.add("/tmp/protected").unwrap();
    assert!(namespace.denies_write(&GuestPath::new("/tmp/protected/file").unwrap(), true, &subtrees));

    assert!(namespace.denies_write_bytes(&GuestPathBytes::new(b"/etc/config").unwrap(), true, &subtrees,));
    assert!(!namespace.denies_write_bytes(&GuestPathBytes::new(b"/tmp/file").unwrap(), true, &subtrees,));
    assert!(namespace.denies_write_bytes(&GuestPathBytes::new(b"/tmp/protected/file").unwrap(), true, &subtrees,));
}
