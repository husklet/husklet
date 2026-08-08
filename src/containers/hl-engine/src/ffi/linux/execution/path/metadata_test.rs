use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;

use super::metadata::{HostMetadata, Node, Registry};
use hl_linux::{AccessPlan, PathOperand};
use hl_runtime::{Access, GuestPath, GuestPathBytes, OpenDirectory, ResolvedPathLease};

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[test]
fn nofollow_access_checks_the_symlink_node() {
    let root = std::env::temp_dir().join(format!("hl-access-link-{}", std::process::id()));
    let link = root.join("dangling");
    std::fs::create_dir_all(&root).unwrap();
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink("missing", &link).unwrap();
    let node = Node::new(
        link.clone(),
        GuestPath::new("/dangling").unwrap(),
        std::sync::Arc::new(Registry::default()),
    );
    let plan = |nofollow| AccessPlan {
        operand: PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(b"/dangling").unwrap(),
            allow_empty: false,
            nofollow,
        },
        access: Access::from_bits(0).unwrap(),
        effective_ids: false,
    };
    assert!(node.access(&plan(false)).is_err());
    assert_eq!(node.access(&plan(true)), Ok(()));
    std::fs::remove_file(link).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn resolved_metadata_extensions() {
    let path = std::env::temp_dir().join(format!("hl-statx-meta-{}", std::process::id()));
    let file = std::fs::File::create(&path).unwrap();
    let path_metadata = HostMetadata::path_resolved(&path).unwrap();
    let file_metadata = HostMetadata::file(&file).unwrap();
    let descriptor_metadata = HostMetadata::file_resolved(&file, file_metadata).unwrap();
    assert!(path_metadata.birth.is_some());
    assert_eq!(path_metadata.birth, descriptor_metadata.birth);
    assert!(path_metadata.mount.is_some());
    assert_eq!(path_metadata.mount, descriptor_metadata.mount);
    std::fs::remove_file(path).unwrap();
}

fn layer(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "hl-anchor-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn lease(upper: &std::path::Path, lower: &std::path::Path) -> super::overlay_lease::ParentLease {
    let guest = hl_runtime::GuestPathBytes::new(b"/").unwrap();
    let upper = std::os::fd::OwnedFd::from(std::fs::File::open(upper).unwrap());
    let lower = std::os::fd::OwnedFd::from(std::fs::File::open(lower).unwrap());
    super::overlay_lease::ParentLease::upper(guest, upper).with_lower_parents(vec![lower])
}

#[test]
fn anchored_metadata_reads_the_visible_layer() {
    let upper = layer("upper");
    let lower = layer("lower");
    std::fs::write(lower.join("only-lower"), b"lower").unwrap();
    std::fs::write(upper.join("both"), b"upper-wins").unwrap();
    std::fs::write(lower.join("both"), b"lower").unwrap();
    let parent = lease(&upper, &lower);

    let hidden = HostMetadata::anchored(&parent, &std::ffi::CString::new("only-lower").unwrap()).unwrap();
    let physical = std::fs::symlink_metadata(lower.join("only-lower")).unwrap();
    assert_eq!(hidden.identity.inode, physical.ino());
    assert_eq!(hidden.size, 5);

    let shadowed = HostMetadata::anchored(&parent, &std::ffi::CString::new("both").unwrap()).unwrap();
    assert_eq!(shadowed.size, 10);

    drop(parent);
    std::fs::remove_dir_all(upper).unwrap();
    std::fs::remove_dir_all(lower).unwrap();
}

#[test]
fn anchored_metadata_stops_at_a_whiteout() {
    let upper = layer("wh-upper");
    let lower = layer("wh-lower");
    std::fs::write(lower.join("gone"), b"lower").unwrap();
    std::fs::write(upper.join(".wh.gone"), b"").unwrap();
    let parent = lease(&upper, &lower);
    assert_eq!(
        HostMetadata::anchored(&parent, &std::ffi::CString::new("gone").unwrap()),
        Err(hl_runtime::RuntimePathError::NotFound)
    );
    drop(parent);
    std::fs::remove_dir_all(upper).unwrap();
    std::fs::remove_dir_all(lower).unwrap();
}

#[test]
fn anchored_metadata_does_not_follow_a_leaf_symlink() {
    let upper = layer("link-upper");
    let lower = layer("link-lower");
    std::os::unix::fs::symlink("missing", upper.join("dangling")).unwrap();
    let parent = lease(&upper, &lower);
    let value = HostMetadata::anchored(&parent, &std::ffi::CString::new("dangling").unwrap()).unwrap();
    assert_eq!(value.kind, hl_runtime::FileKind::Symlink);
    drop(parent);
    std::fs::remove_dir_all(upper).unwrap();
    std::fs::remove_dir_all(lower).unwrap();
}

#[test]
fn anchored_resolved_matches_the_rendered_path() {
    let upper = layer("ext-upper");
    let lower = layer("ext-lower");
    std::fs::write(lower.join("file"), b"data").unwrap();
    let parent = lease(&upper, &lower);
    let name = std::ffi::CString::new("file").unwrap();
    let anchored = HostMetadata::anchored_resolved(&parent, &name).unwrap();
    let rendered = HostMetadata::path_resolved(&lower.join("file")).unwrap();
    assert_eq!(anchored.birth, rendered.birth);
    assert_eq!(anchored.mount, rendered.mount);
    assert_eq!(anchored.file, rendered.file);
    assert!(anchored.birth.is_some());
    drop(parent);
    std::fs::remove_dir_all(upper).unwrap();
    std::fs::remove_dir_all(lower).unwrap();
}

#[test]
fn ownership_hardlink() {
    let root = std::env::temp_dir().join(format!("hl-owner-{}", std::process::id()));
    let source = root.join("source");
    let alias = root.join("alias");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&source, b"data").unwrap();
    std::fs::hard_link(&source, &alias).unwrap();
    let file = std::fs::File::open(&source).unwrap();
    let host = file.metadata().unwrap();
    let user = host.uid().wrapping_add(10_000);
    let group = host.gid().wrapping_add(20_000);
    let registry = Registry::default();
    registry.set(file.as_raw_fd(), user, group).unwrap();

    for path in [&source, &alias] {
        let mut projected = HostMetadata::path(path).unwrap();
        registry.project(&mut projected);
        assert_eq!((projected.user, projected.group), (user, group));
        let physical = std::fs::metadata(path).unwrap();
        assert_eq!((physical.uid(), physical.gid()), (host.uid(), host.gid()));
    }

    drop(file);
    std::fs::remove_dir_all(root).unwrap();
}
