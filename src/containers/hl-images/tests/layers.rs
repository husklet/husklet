use std::io::Cursor;

use hl_images::{layer::Layer, Error};

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut bytes);
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, *content).unwrap();
        }
        tar.finish().unwrap();
    }
    bytes
}

fn archive_with_forward_hardlink(target_present: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut bytes);
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Link);
        link.set_size(0);
        link.set_mode(0o644);
        link.set_link_name("target").unwrap();
        link.set_cksum();
        tar.append_data(&mut link, "link", &[][..]).unwrap();
        if target_present {
            let mut target = tar::Header::new_gnu();
            target.set_size(7);
            target.set_mode(0o644);
            target.set_cksum();
            tar.append_data(&mut target, "target", &b"content"[..])
                .unwrap();
        }
        tar.finish().unwrap();
    }
    bytes
}

fn archive_with_root() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut bytes);
        let mut root = tar::Header::new_gnu();
        root.set_entry_type(tar::EntryType::Directory);
        root.set_size(0);
        root.set_mode(0o755);
        root.set_cksum();
        tar.append_data(&mut root, "./", &[][..]).unwrap();

        let mut file = tar::Header::new_gnu();
        file.set_size(2);
        file.set_mode(0o644);
        file.set_cksum();
        tar.append_data(&mut file, "./etc/ok", &b"ok"[..]).unwrap();
        tar.finish().unwrap();
    }
    bytes
}

fn archive_with_device(path: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut bytes);
        let mut device = tar::Header::new_gnu();
        device.set_entry_type(tar::EntryType::Char);
        device.set_size(0);
        device.set_mode(0o600);
        device.set_device_major(5).unwrap();
        device.set_device_minor(1).unwrap();
        device.set_cksum();
        tar.append_data(&mut device, path, &[][..]).unwrap();

        let mut file = tar::Header::new_gnu();
        file.set_size(2);
        file.set_mode(0o644);
        file.set_cksum();
        tar.append_data(&mut file, "ok", &b"ok"[..]).unwrap();
        tar.finish().unwrap();
    }
    bytes
}

fn archive_with_replaced_directory() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut bytes);
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_size(0);
        directory.set_mode(0o700);
        directory.set_cksum();
        tar.append_data(&mut directory, "usr/bin/tool", &[][..])
            .unwrap();

        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        link.set_link_name("missing-target").unwrap();
        link.set_cksum();
        tar.append_data(&mut link, "usr/bin/tool", &[][..]).unwrap();
        tar.finish().unwrap();
    }
    bytes
}

fn archive_with_node_kinds() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut bytes);
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_size(0);
        directory.set_mode(0o755);
        directory.set_cksum();
        tar.append_data(&mut directory, "tree", &[][..]).unwrap();

        let mut file = tar::Header::new_gnu();
        file.set_size(7);
        file.set_mode(0o644);
        file.set_cksum();
        tar.append_data(&mut file, "tree/file", &b"content"[..]).unwrap();

        let mut symlink = tar::Header::new_gnu();
        symlink.set_entry_type(tar::EntryType::Symlink);
        symlink.set_size(0);
        symlink.set_mode(0o777);
        symlink.set_link_name("file").unwrap();
        symlink.set_cksum();
        tar.append_data(&mut symlink, "tree/symlink", &[][..]).unwrap();

        let mut hardlink = tar::Header::new_gnu();
        hardlink.set_entry_type(tar::EntryType::Link);
        hardlink.set_size(0);
        hardlink.set_mode(0o644);
        hardlink.set_link_name("tree/file").unwrap();
        hardlink.set_cksum();
        tar.append_data(&mut hardlink, "tree/hardlink", &[][..]).unwrap();
        tar.finish().unwrap();
    }
    bytes
}

#[test]
fn diff_size_matches_moby_header_content_accounting() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = archive_with_node_kinds();
    let report = Layer::new(Cursor::new(bytes.clone())).apply(temp.path()).unwrap();

    assert_eq!(report.diff_size.bytes(), 7);
    assert!(bytes.len() > report.diff_size.bytes() as usize);
}

#[test]
fn diff_size_counts_regular_payloads_and_excludes_whiteout_headers() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("old"), b"old").unwrap();
    let report = Layer::new(Cursor::new(archive(&[(".wh.old", b""), ("new", b"payload")])))
        .apply(temp.path())
        .unwrap();

    assert_eq!(report.diff_size.bytes(), 7);
}

#[test]
fn root_directory_markers_are_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let report = Layer::new(Cursor::new(archive_with_root()))
        .apply(temp.path())
        .unwrap();
    assert_eq!(report.entries, 1);
    assert_eq!(std::fs::read(temp.path().join("etc/ok")).unwrap(), b"ok");
}

#[test]
fn runtime_owned_device_nodes_are_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let report = Layer::new(Cursor::new(archive_with_device("dev/console")))
        .apply(temp.path())
        .unwrap();
    assert_eq!(report.entries, 1);
    assert!(!temp.path().join("dev/console").exists());
    assert_eq!(std::fs::read(temp.path().join("ok")).unwrap(), b"ok");
}

#[test]
fn special_nodes_outside_dev_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let result = Layer::new(Cursor::new(archive_with_device("etc/console"))).apply(temp.path());
    assert!(matches!(result, Err(Error::UnsafeArchive { .. })));
    assert!(!temp.path().join("etc/console").exists());
}

#[cfg(unix)]
#[test]
fn readonly_parent_is_temporarily_writable_and_restored() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("locked");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

    Layer::new(Cursor::new(archive(&[("locked/value", b"ok")])))
        .apply(temp.path())
        .unwrap();

    assert_eq!(std::fs::read(parent.join("value")).unwrap(), b"ok");
    assert_eq!(
        std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o555
    );
}

#[test]
fn whiteouts_remove_lower_files() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("old"), "old").unwrap();
    let report = Layer::new(Cursor::new(archive(&[(".wh.old", b""), ("new", b"new")])))
        .apply(temp.path())
        .unwrap();
    assert_eq!(report.entries, 1);
    assert_eq!(report.whiteouts, 1);
    assert!(!temp.path().join("old").exists());
    assert_eq!(std::fs::read(temp.path().join("new")).unwrap(), b"new");
}

#[test]
fn empty_whiteout_is_consumed_without_removing_its_parent() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("directory")).unwrap();
    std::fs::write(temp.path().join("directory/safe"), "safe").unwrap();
    let report = Layer::new(Cursor::new(archive(&[("directory/.wh.", b"")])))
        .apply(temp.path())
        .unwrap();
    assert_eq!(report.whiteouts, 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("directory/safe")).unwrap(),
        "safe"
    );
    assert!(!temp.path().join("directory/.wh.").exists());
}

#[test]
fn whiteouts_remove_directory_trees_at_any_depth() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("top/tree/nested")).unwrap();
    std::fs::write(temp.path().join("top/tree/nested/file"), "old").unwrap();
    std::fs::write(temp.path().join("top/keep"), "keep").unwrap();
    let report = Layer::new(Cursor::new(archive(&[("top/.wh.tree", b"")])))
        .apply(temp.path())
        .unwrap();
    assert_eq!(report.whiteouts, 1);
    assert!(!temp.path().join("top/tree").exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("top/keep")).unwrap(),
        "keep"
    );
}

#[test]
fn opaque_whiteout_replaces_lower_directory_contents() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("data/nested")).unwrap();
    std::fs::write(temp.path().join("data/old"), "old").unwrap();
    std::fs::write(temp.path().join("data/nested/old"), "old").unwrap();
    let report = Layer::new(Cursor::new(archive(&[
        ("data/.wh..wh..opq", b""),
        ("data/current", b"current"),
    ])))
    .apply(temp.path())
    .unwrap();
    assert_eq!(report.whiteouts, 1);
    assert!(!temp.path().join("data/old").exists());
    assert!(!temp.path().join("data/nested").exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("data/current")).unwrap(),
        "current"
    );
    assert!(!temp.path().join("data/.wh..wh..opq").exists());
}

#[cfg(unix)]
#[test]
fn deferred_directory_metadata_does_not_follow_a_replacement_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let report = Layer::new(Cursor::new(archive_with_replaced_directory()))
        .apply(temp.path())
        .unwrap();

    assert_eq!(report.entries, 2);
    let tool = temp.path().join("usr/bin/tool");
    assert!(std::fs::symlink_metadata(&tool)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_link(tool).unwrap(),
        std::path::Path::new("missing-target")
    );
}

#[test]
fn traversal_and_symlink_escape_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    // tar's builder rejects traversal itself, so construct a safe symlink followed by traversal through it.
    #[cfg(unix)]
    std::os::unix::fs::symlink("/tmp", temp.path().join("escape")).unwrap();
    let result = Layer::new(Cursor::new(archive(&[("escape/pwn", b"no")]))).apply(temp.path());
    assert!(matches!(result, Err(Error::UnsafeArchive { .. })));
}

#[test]
fn replacing_a_lower_symlink_never_writes_through_it() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), root.path().join("victim")).unwrap();
    Layer::new(Cursor::new(archive(&[("victim", b"inside")])))
        .apply(root.path())
        .unwrap();
    assert_eq!(std::fs::read_to_string(outside.path()).unwrap(), "outside");
    assert_eq!(
        std::fs::read_to_string(root.path().join("victim")).unwrap(),
        "inside"
    );
    assert!(!std::fs::symlink_metadata(root.path().join("victim"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn forward_hardlink_is_resolved_after_the_complete_layer() {
    use std::os::unix::fs::MetadataExt;

    let root = tempfile::tempdir().unwrap();
    Layer::new(Cursor::new(archive_with_forward_hardlink(true)))
        .apply(root.path())
        .unwrap();
    assert_eq!(std::fs::read(root.path().join("link")).unwrap(), b"content");
    assert_eq!(
        std::fs::metadata(root.path().join("link")).unwrap().ino(),
        std::fs::metadata(root.path().join("target")).unwrap().ino()
    );
}

#[test]
fn unresolved_forward_hardlink_fails_after_layer_closure() {
    let root = tempfile::tempdir().unwrap();
    let error = Layer::new(Cursor::new(archive_with_forward_hardlink(false)))
        .apply(root.path())
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("was not present after applying the complete layer"));
}
