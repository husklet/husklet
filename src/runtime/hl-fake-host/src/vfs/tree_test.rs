use crate::{FakeHost, Fault, VfsTree};
use hl_vfs::{GuestName, NodeKind, ResolveHostError, VfsHost, XattrName};

#[test]
fn tree_metadata_links() {
    let tree = VfsTree::new(FakeHost::new(1));
    let root = tree.resolve("/").unwrap();
    let directory = tree.mkdir(root, "dir").unwrap();
    let file = tree.create_file(directory, "b", vec![1, 2]).unwrap();
    tree.create_file(directory, "a", vec![]).unwrap();
    tree.set_permissions(file, 0o640).unwrap();
    tree.set_owner(file, 5, 6).unwrap();
    let xattr = XattrName::new(b"user.\xff").unwrap();
    tree.set_xattr(file, &xattr, vec![9]).unwrap();
    tree.link(directory, "hard", file).unwrap();
    assert_eq!(tree.metadata(file).unwrap().links, 2);
    assert_eq!(tree.read_file(file).unwrap(), [1, 2]);
    assert_eq!(tree.xattr(file, &xattr).unwrap(), Some(vec![9]));
    let names: Vec<_> = tree.directory(directory).into_iter().map(|row| row.1).collect();
    assert_eq!(names, ["a", "b", "hard"]);
    assert!(
        tree.watch_events()
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[test]
fn rename_is_atomic() {
    let host = FakeHost::new(2);
    let tree = VfsTree::new(host.clone());
    let root = tree.resolve("/").unwrap();
    tree.create_file(root, "source", vec![]).unwrap();
    host.fail_at(host.transcript().len() + 1, Fault::Failed);
    assert!(tree.rename(root, "source", root, "target").is_err());
    assert!(tree.resolve("/source").is_ok());
    assert!(tree.resolve("/target").is_err());
    let clean_tree = VfsTree::new(FakeHost::new(3));
    let root = clean_tree.resolve("/").unwrap();
    clean_tree.symlink(root, "escape", b"/../outside").unwrap();
    assert!(clean_tree.resolve("/escape").is_err());
}

#[test]
fn pinned_symlink_survives() {
    let host = FakeHost::new(4);
    let tree = VfsTree::new(host.clone());
    let root_identity = tree.resolve("/").unwrap();
    tree.symlink(root_identity, "old", b"/destination").unwrap();
    let root = tree.pin_root().unwrap();
    let old = GuestName::new(b"old").unwrap();
    let (link, kind) = tree.inspect_child(root, &old).unwrap();
    assert_eq!(kind, NodeKind::Symlink);
    tree.rename(root_identity, "old", root_identity, "new").unwrap();
    tree.unlink(root_identity, "new").unwrap();
    let mut target = [0; 32];
    let count = tree.read_link(link, &mut target).unwrap();
    assert_eq!(&target[..count], b"/destination");
    tree.close(link);
    tree.close(root);
    assert!(host.resources().is_empty());
}

#[test]
fn resolver_adapter_accepts() {
    let tree = VfsTree::new(FakeHost::new(5));
    let root = tree.pin_root().unwrap();
    let invalid = GuestName::new(b"\xff").unwrap();

    assert_eq!(
        tree.inspect_child(root, &invalid).unwrap_err(),
        ResolveHostError::NotFound
    );
    tree.close(root);
}
