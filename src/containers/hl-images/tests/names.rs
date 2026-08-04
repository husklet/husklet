use std::{fs, path::Path};

use hl_images::layer::Layer;
use hl_images::snapshot::{Id, Snapshots};

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

#[test]
fn encoded_names_survive_commit_reopen_and_fork() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active"), None).unwrap();
    let physical = draft.names_mut().encode(Path::new("data/FOO")).unwrap();
    assert_eq!(
        physical,
        Path::new("data/.hl-name-e8ddf619307e1c81f8142d012395031ce18df9ec419e082a88af1acbb31949bc")
    );
    assert_eq!(draft.names().physical(Path::new("data/FOO")), physical);
    assert_eq!(draft.names().guest(&physical), Path::new("data/FOO"));
    fs::create_dir_all(draft.path().join("data")).unwrap();
    fs::write(draft.path().join(&physical), b"upper").unwrap();
    draft.commit(id("parent")).unwrap();

    let reopened = Snapshots::open(temp.path()).unwrap();
    let view = reopened.view(&id("parent")).unwrap();
    assert_eq!(view.names().physical(Path::new("data/FOO")), physical);
    let child = reopened.prepare(id("child-active"), Some(&id("parent"))).unwrap();
    assert_eq!(child.names().guest(&physical), Path::new("data/FOO"));
    child.commit(id("child")).unwrap();
    assert!(temp.path().join("names/committed/child.json").is_file());
}

#[test]
fn raw_names_are_not_recorded_and_sidecars_follow_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let draft = snapshots.prepare(id("abort"), None).unwrap();
    assert_eq!(draft.names().physical(Path::new("raw")), Path::new("raw"));
    assert_eq!(draft.names().iter().count(), 0);
    draft.abort().unwrap();
    assert!(!temp.path().join("names/active/abort.json").exists());

    snapshots
        .prepare(id("active"), None)
        .unwrap()
        .commit(id("done"))
        .unwrap();
    assert!(snapshots.remove(&id("done")).unwrap());
    assert!(!temp.path().join("names/committed/done.json").exists());
}

#[test]
fn opening_another_store_preserves_a_live_draft() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let draft = snapshots.prepare(id("worker-one"), None).unwrap();
    fs::write(draft.path().join("marker"), b"live").unwrap();

    let other = Snapshots::open(temp.path()).unwrap();
    assert_eq!(fs::read(draft.path().join("marker")).unwrap(), b"live");
    other.prepare(id("worker-two"), None).unwrap().abort().unwrap();
    draft.abort().unwrap();
}

#[test]
fn concurrent_stores_do_not_delete_each_others_drafts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_owned();
    let workers = (0..8)
        .map(|worker| {
            let root = root.clone();
            std::thread::spawn(move || {
                let snapshots = Snapshots::open(root).unwrap();
                let draft = snapshots.prepare(id(&format!("worker-{worker}")), None).unwrap();
                fs::write(draft.path().join("marker"), worker.to_string()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(20));
                assert_eq!(
                    fs::read_to_string(draft.path().join("marker")).unwrap(),
                    worker.to_string()
                );
                draft.abort().unwrap();
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(fs::read_dir(root.join("active")).unwrap().count(), 0);
}

#[test]
fn malformed_names_sidecar_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    snapshots
        .prepare(id("active"), None)
        .unwrap()
        .commit(id("bad"))
        .unwrap();
    fs::write(temp.path().join("names/committed/bad.json"), br#"{"literal":"guest"}"#).unwrap();
    assert!(snapshots.view(&id("bad")).is_err());
    assert!(snapshots.prepare(id("fork"), Some(&id("bad"))).is_err());
    assert!(!temp.path().join("active/fork").exists());
    assert!(!temp.path().join("ownership/active/fork.json").exists());
    assert!(!temp.path().join("names/active/fork.json").exists());
}

#[test]
fn case_distinct_layer_names_persist_and_export_as_guest_names() {
    let mut tar = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut tar);
        for (name, body) in [("data/foo", b"lower".as_slice()), ("data/FOO", b"upper".as_slice())] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, name, body).unwrap();
        }
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Link);
        link.set_size(0);
        link.set_mode(0o644);
        link.set_link_name("data/FOO").unwrap();
        link.set_cksum();
        archive.append_data(&mut link, "data/link", &[][..]).unwrap();
        let mut symlink = tar::Header::new_gnu();
        symlink.set_entry_type(tar::EntryType::Symlink);
        symlink.set_size(0);
        symlink.set_mode(0o777);
        symlink.set_link_name("FOO").unwrap();
        symlink.set_cksum();
        archive.append_data(&mut symlink, "data/symlink", &[][..]).unwrap();
        archive.finish().unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active"), None).unwrap();
    let root = draft.path().to_owned();
    let (owners, names) = draft.metadata_mut();
    Layer::new(tar.as_slice())
        .apply_with_metadata(&root, owners, names)
        .unwrap();
    let physical = draft.names().physical(Path::new("data/FOO")).to_owned();
    assert_ne!(physical, Path::new("data/FOO"));
    assert_eq!(fs::read(root.join("data/foo")).unwrap(), b"lower");
    assert_eq!(fs::read(root.join(&physical)).unwrap(), b"upper");
    assert_eq!(fs::read(root.join("data/link")).unwrap(), b"upper");
    assert_eq!(fs::read(root.join("data/symlink")).unwrap(), b"upper");
    let view = draft.commit(id("committed")).unwrap();
    let mut exported = Vec::new();
    view.archive(&mut exported).unwrap();
    let mut paths = tar::Archive::new(exported.as_slice())
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(
        paths,
        vec!["data", "data/FOO", "data/foo", "data/link", "data/symlink"]
            .into_iter()
            .map(Into::into)
            .collect::<Vec<std::path::PathBuf>>()
    );
    let target = tar::Archive::new(exported.as_slice())
        .entries()
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.path().unwrap() == Path::new("data/symlink"))
        .unwrap()
        .link_name()
        .unwrap()
        .unwrap()
        .into_owned();
    assert_eq!(target, Path::new("FOO"));
}

#[cfg(unix)]
#[test]
fn hardlink_resolves_through_a_case_distinct_directory() {
    use std::os::unix::fs::MetadataExt;

    let mut tar = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut tar);
        for name in ["usr/share/terminfo/L", "usr/share/terminfo/l"] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            archive.append_data(&mut header, name, &[][..]).unwrap();
            if name.ends_with("/L") {
                let mut file = tar::Header::new_gnu();
                file.set_size(7);
                file.set_mode(0o644);
                file.set_cksum();
                archive
                    .append_data(&mut file, "usr/share/terminfo/L/LFT-PC850", &b"content"[..])
                    .unwrap();
            }
        }
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Link);
        link.set_size(0);
        link.set_mode(0o644);
        link.set_link_name("usr/share/terminfo/L/LFT-PC850").unwrap();
        link.set_cksum();
        archive
            .append_data(&mut link, "usr/share/terminfo/l/lft-pc850", &[][..])
            .unwrap();
        archive.finish().unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active-hardlink"), None).unwrap();
    let root = draft.path().to_owned();
    let (owners, names) = draft.metadata_mut();
    Layer::new(tar.as_slice())
        .apply_with_metadata(&root, owners, names)
        .unwrap();
    let upper = draft.names().physical(Path::new("usr/share/terminfo/L/LFT-PC850"));
    let lower = draft.names().physical(Path::new("usr/share/terminfo/l/lft-pc850"));
    assert_eq!(
        fs::metadata(root.join(upper)).unwrap().ino(),
        fs::metadata(root.join(lower)).unwrap().ino()
    );
}

#[cfg(unix)]
#[test]
fn metadata_layer_preserves_same_directory_symlink_target() {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name(".").unwrap();
        header.set_cksum();
        archive.append_data(&mut header, "usr/bin/X11", &[][..]).unwrap();
        archive.finish().unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active"), None).unwrap();
    let root = draft.path().to_owned();
    let (owners, names) = draft.metadata_mut();
    Layer::new(bytes.as_slice())
        .apply_with_metadata(&root, owners, names)
        .unwrap();
    assert_eq!(fs::read_link(root.join("usr/bin/X11")).unwrap(), Path::new("."));
}

#[test]
fn whiteout_removes_only_the_case_exact_encoded_guest() {
    fn layer(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut archive = tar::Builder::new(&mut bytes);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, name, *body).unwrap();
        }
        archive.finish().unwrap();
        drop(archive);
        bytes
    }
    let temp = tempfile::tempdir().unwrap();
    let snapshots = Snapshots::open(temp.path()).unwrap();
    let mut draft = snapshots.prepare(id("active"), None).unwrap();
    let root = draft.path().to_owned();
    let initial = layer(&[("data/foo", b"lower"), ("data/FOO", b"upper")]);
    let (owners, names) = draft.metadata_mut();
    Layer::new(initial.as_slice())
        .apply_with_metadata(&root, owners, names)
        .unwrap();
    let physical = draft.names().physical(Path::new("data/FOO")).to_owned();

    let whiteout = layer(&[("data/.wh.FOO", b"")]);
    let (owners, names) = draft.metadata_mut();
    Layer::new(whiteout.as_slice())
        .apply_with_metadata(&root, owners, names)
        .unwrap();
    assert_eq!(fs::read(root.join("data/foo")).unwrap(), b"lower");
    assert!(!root.join(physical).exists());
}
