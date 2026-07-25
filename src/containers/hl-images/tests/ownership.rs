use std::fs;

use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

use hl_images::layer::Layer;
use hl_images::snapshot::{Id, Ownership, Snapshots};
use hl_images::{Images, Platform, Reference, RuntimeConfig};

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

#[test]
fn ownership_survives_commit_reopen_and_fork_by_entry_path() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut parent = snapshots.prepare(id("parent-active"), None).unwrap();

    fs::write(parent.path().join("file"), b"data").unwrap();
    std::os::unix::fs::symlink("file", parent.path().join("symlink")).unwrap();
    fs::hard_link(parent.path().join("file"), parent.path().join("hardlink")).unwrap();

    let file = Ownership { uid: 1000, gid: 10 };
    let symlink = Ownership { uid: 1001, gid: 11 };
    let hardlink = Ownership { uid: 1002, gid: 12 };
    parent.ownership_mut().set("file", file).unwrap();
    parent.ownership_mut().set("symlink", symlink).unwrap();
    parent.ownership_mut().set("hardlink", hardlink).unwrap();
    let parent_id = id("parent");
    parent.commit(parent_id.clone()).unwrap();

    let reopened = Snapshots::open(temp.path()).unwrap();
    let view = reopened.view(&parent_id).unwrap();
    assert_eq!(view.ownership().get("file"), Some(file));
    assert_eq!(view.ownership().get("symlink"), Some(symlink));
    assert_eq!(view.ownership().get("hardlink"), Some(hardlink));

    let child = reopened
        .prepare(id("child-active"), Some(&parent_id))
        .unwrap();
    assert_eq!(
        fs::metadata(child.path().join("file")).unwrap().ino(),
        fs::metadata(child.path().join("hardlink")).unwrap().ino()
    );
    assert_eq!(child.ownership().get("file"), Some(file));
    assert_eq!(child.ownership().get("symlink"), Some(symlink));
    assert_eq!(child.ownership().get("hardlink"), Some(hardlink));
    let child = child.commit(id("child")).unwrap();
    assert_eq!(child.ownership().get("file"), Some(file));
}

#[test]
fn abort_drop_and_remove_delete_their_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();

    let aborted = snapshots.prepare(id("aborted"), None).unwrap();
    aborted.abort().unwrap();
    assert!(!temp.path().join("ownership/active/aborted.json").exists());

    let dropped = snapshots.prepare(id("dropped"), None).unwrap();
    drop(dropped);
    assert!(!temp.path().join("ownership/active/dropped.json").exists());

    let committed = snapshots.prepare(id("active"), None).unwrap();
    committed.commit(id("committed")).unwrap();
    assert!(temp
        .path()
        .join("ownership/committed/committed.json")
        .exists());
    assert!(snapshots.remove(&id("committed")).unwrap());
    assert!(!temp
        .path()
        .join("ownership/committed/committed.json")
        .exists());
}

#[test]
fn malformed_and_unsafe_sidecars_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    snapshots
        .prepare(id("active"), None)
        .unwrap()
        .commit(id("bad"))
        .unwrap();
    let sidecar = temp.path().join("ownership/committed/bad.json");

    fs::write(&sidecar, b"not-json").unwrap();
    assert!(snapshots.view(&id("bad")).is_err());

    fs::write(&sidecar, br#"{"../escape":{"uid":1,"gid":2}}"#).unwrap();
    assert!(snapshots.view(&id("bad")).is_err());
    assert!(snapshots.prepare(id("fork"), Some(&id("bad"))).is_err());
    assert!(!temp.path().join("active/fork").exists());
}

#[test]
fn ownership_rejects_non_normalized_paths_without_mutating_state() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active"), None).unwrap();
    let owner = Ownership { uid: 1, gid: 2 };

    assert!(draft.ownership_mut().set("../escape", owner).is_err());
    assert!(draft.ownership_mut().set("a/./b", owner).is_err());
    assert!(draft.ownership_mut().set("/absolute", owner).is_err());
    assert_eq!(draft.ownership().get("escape"), None);
}

#[cfg(unix)]
#[test]
fn fork_traverses_readonly_parent_and_restores_its_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let parent = snapshots.prepare(id("parent-active"), None).unwrap();
    let locked = parent.path().join("var/log/faillock");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("record"), b"locked").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o0)).unwrap();
    parent.commit(id("parent")).unwrap();

    let child = snapshots
        .prepare(id("child-active"), Some(&id("parent")))
        .unwrap();
    assert_eq!(
        fs::symlink_metadata(temp.path().join("committed/parent/var/log/faillock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0
    );
    assert_eq!(
        fs::symlink_metadata(child.path().join("var/log/faillock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0
    );
    fs::set_permissions(
        child.path().join("var/log/faillock"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_eq!(
        fs::read(child.path().join("var/log/faillock/record")).unwrap(),
        b"locked"
    );
}

#[test]
fn layer_import_records_guest_ownership_for_each_entry_name() {
    use std::io::Cursor;

    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        {
            let mut append = |path: &str, body: &[u8], uid: u64, gid: u64| {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_uid(uid);
                header.set_gid(gid);
                header.set_cksum();
                archive.append_data(&mut header, path, body).unwrap();
            };
            append("file", b"data", 1000, 100);
        }

        let mut symlink = tar::Header::new_gnu();
        symlink.set_entry_type(tar::EntryType::Symlink);
        symlink.set_size(0);
        symlink.set_uid(1001);
        symlink.set_gid(101);
        symlink.set_link_name("file").unwrap();
        symlink.set_cksum();
        archive
            .append_data(&mut symlink, "symlink", &[][..])
            .unwrap();

        let mut hardlink = tar::Header::new_gnu();
        hardlink.set_entry_type(tar::EntryType::Link);
        hardlink.set_size(0);
        hardlink.set_uid(1002);
        hardlink.set_gid(102);
        hardlink.set_link_name("file").unwrap();
        hardlink.set_cksum();
        archive
            .append_data(&mut hardlink, "hardlink", &[][..])
            .unwrap();
        archive.finish().unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active"), None).unwrap();
    let root = draft.path().to_owned();
    Layer::new(Cursor::new(bytes))
        .apply_with_ownership(&root, draft.ownership_mut())
        .unwrap();
    let view = draft.commit(id("committed")).unwrap();

    assert_eq!(
        view.ownership().get("file"),
        Some(Ownership {
            uid: 1000,
            gid: 100
        })
    );
    assert_eq!(
        view.ownership().get("symlink"),
        Some(Ownership {
            uid: 1001,
            gid: 101
        })
    );
    assert_eq!(
        view.ownership().get("hardlink"),
        Some(Ownership {
            uid: 1002,
            gid: 102
        })
    );

    let mut exported = Vec::new();
    view.archive(&mut exported).unwrap();
    let mut headers = tar::Archive::new(exported.as_slice())
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.path().unwrap().into_owned(),
                entry.header().uid().unwrap(),
                entry.header().gid().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    headers.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        headers,
        vec![
            ("file".into(), 1000, 100),
            ("hardlink".into(), 1002, 102),
            ("symlink".into(), 1001, 101),
        ]
    );
}

#[test]
fn failed_unpack_publishes_no_snapshot_and_reopen_retries_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = RuntimeConfig {
        entrypoint: vec!["/bin/sh".into()],
        command: Vec::new(),
        environment: BTreeMap::new(),
        working_directory: "/".into(),
        user: String::new(),
    };
    let name: Reference = "example.test/broken:latest".parse().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .commit(
            b"not a tar stream",
            &runtime,
            &Platform::linux_arm64(),
            &name,
        )
        .unwrap();

    let first = images
        .unpack(&image, &Platform::linux_arm64())
        .unwrap_err()
        .to_string();
    assert!(!first.contains("parent snapshot does not exist"));
    assert_eq!(
        fs::read_dir(temp.path().join("snapshots/committed"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fs::read_dir(temp.path().join("snapshots/active"))
            .unwrap()
            .count(),
        0
    );

    let reopened = Images::open(temp.path()).unwrap();
    let cached = reopened.resolve(&name).unwrap().unwrap();
    let second = reopened
        .unpack(&cached, &Platform::linux_arm64())
        .unwrap_err()
        .to_string();
    assert!(!second.contains("parent snapshot does not exist"));
    assert_eq!(
        fs::read_dir(temp.path().join("snapshots/committed"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn failed_unpack_cleans_readonly_draft_before_same_cache_retry() {
    let mut layer = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut layer);
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_size(0);
        directory.set_mode(0);
        directory.set_uid(0);
        directory.set_gid(0);
        directory.set_cksum();
        archive
            .append_data(&mut directory, "locked", &[][..])
            .unwrap();
        let mut fifo = tar::Header::new_gnu();
        fifo.set_entry_type(tar::EntryType::Fifo);
        fifo.set_size(0);
        fifo.set_mode(0o600);
        fifo.set_uid(0);
        fifo.set_gid(0);
        fifo.set_cksum();
        archive
            .append_data(&mut fifo, "forbidden", &[][..])
            .unwrap();
        archive.finish().unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let images = Images::open(temp.path()).unwrap();
    let image = images
        .commit(
            &layer,
            &RuntimeConfig {
                entrypoint: Vec::new(),
                command: Vec::new(),
                environment: BTreeMap::new(),
                working_directory: "/".into(),
                user: String::new(),
            },
            &Platform::linux_arm64(),
            &"example.test/readonly-failure:latest".parse().unwrap(),
        )
        .unwrap();

    for _ in 0..2 {
        assert!(images.unpack(&image, &Platform::linux_arm64()).is_err());
        assert_eq!(
            fs::read_dir(temp.path().join("snapshots/active"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(temp.path().join("snapshots/ownership/active"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn reopening_preserves_drafts_owned_by_other_processes() {
    let temp = tempfile::tempdir().unwrap();
    let active = temp.path().join("active/orphan/locked");
    fs::create_dir_all(&active).unwrap();
    fs::write(active.join("partial"), b"partial").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&active, fs::Permissions::from_mode(0o0)).unwrap();
    }
    fs::create_dir_all(temp.path().join("ownership/active")).unwrap();
    fs::write(
        temp.path().join("ownership/active/orphan.json"),
        br#"{"locked/partial":{"uid":1,"gid":2}}"#,
    )
    .unwrap();

    let snapshots = Snapshots::open(temp.path()).unwrap();
    assert_eq!(fs::read_dir(temp.path().join("active")).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(temp.path().join("ownership/active"))
            .unwrap()
            .count(),
        1
    );
    snapshots
        .prepare(id("fresh"), None)
        .unwrap()
        .commit(id("committed"))
        .unwrap();
}

#[test]
fn recursive_ownership_stops_at_symlinks_and_exports_each_entry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir_all(root.join("tree/child")).unwrap();
    fs::write(root.join("tree/child/file"), b"inside").unwrap();
    fs::write(temp.path().join("outside"), b"outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("../../../outside", root.join("tree/link")).unwrap();

    let owner = Ownership { uid: 77, gid: 88 };
    let mut ownerships = hl_images::snapshot::Ownerships::memory();
    ownerships.set_recursive(&root, "tree", owner).unwrap();
    assert_eq!(ownerships.get("tree"), Some(owner));
    assert_eq!(ownerships.get("tree/child"), Some(owner));
    assert_eq!(ownerships.get("tree/child/file"), Some(owner));
    #[cfg(unix)]
    assert_eq!(ownerships.get("tree/link"), Some(owner));
    assert_eq!(ownerships.get("outside"), None);

    let mut bytes = Vec::new();
    ownerships.archive(&root, &mut bytes).unwrap();
    for entry in tar::Archive::new(bytes.as_slice()).entries().unwrap() {
        let entry = entry.unwrap();
        assert_eq!(entry.header().uid().unwrap(), 77);
        assert_eq!(entry.header().gid().unwrap(), 88);
    }
}

#[test]
fn recursive_root_ownership_updates_children_without_an_empty_key() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("directory")).unwrap();
    fs::write(temp.path().join("directory/file"), b"data").unwrap();
    fs::write(temp.path().join("top"), b"data").unwrap();
    let owner = Ownership { uid: 9, gid: 10 };
    let mut ownerships = hl_images::snapshot::Ownerships::memory();
    ownerships.set_recursive(temp.path(), "", owner).unwrap();
    assert_eq!(ownerships.get("directory"), Some(owner));
    assert_eq!(ownerships.get("directory/file"), Some(owner));
    assert_eq!(ownerships.get("top"), Some(owner));
    assert_eq!(ownerships.get(""), None);
}
