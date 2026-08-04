use super::*;

fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o640);
            header.set_cksum();
            archive.append_data(&mut header, path, *contents).unwrap();
        }
        archive.finish().unwrap();
    }
    bytes
}

fn tar_owned(path: &str, contents: &[u8], uid: u64, gid: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut archive = tar::Builder::new(&mut bytes);
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o640);
    header.set_uid(uid);
    header.set_gid(gid);
    header.set_cksum();
    archive.append_data(&mut header, path, contents).unwrap();
    archive.finish().unwrap();
    drop(archive);
    bytes
}

struct OverlayFixture {
    temporary: tempfile::TempDir,
    lower: PathBuf,
    upper: PathBuf,
    filesystem: Filesystem,
}

impl OverlayFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let lower = temporary.path().join("lower");
        let upper = temporary.path().join("upper");
        fs::create_dir_all(lower.join("data/sub")).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(lower.join("data/lower"), b"lower").unwrap();
        fs::write(lower.join("data/copied"), b"old").unwrap();
        fs::write(lower.join("data/deleted"), b"hidden").unwrap();
        fs::create_dir_all(upper.join("data")).unwrap();
        fs::write(upper.join("data/copied"), b"new-value").unwrap();
        fs::write(upper.join("data/.wh.deleted"), b"").unwrap();
        let mut ownership = hl_images::snapshot::Ownerships::memory();
        ownership
            .set("data/lower", hl_images::snapshot::Ownership { uid: 41, gid: 42 })
            .unwrap();
        let filesystem = Filesystem::overlay(
            lower.clone(),
            upper.clone(),
            ownership,
            hl_images::snapshot::Ownerships::memory(),
            Vec::new(),
        );
        Self {
            temporary,
            lower,
            upper,
            filesystem,
        }
    }
}

fn tar_with_directory(path: &str, file: &str, contents: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_size(0);
        directory.set_mode(0o755);
        directory.set_cksum();
        archive.append_data(&mut directory, path, &b""[..]).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o640);
        header.set_cksum();
        archive.append_data(&mut header, file, contents).unwrap();
        archive.finish().unwrap();
    }
    bytes
}

#[test]
fn archives_and_extracts_files_with_mount_routing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let mount = temporary.path().join("mount");
    fs::create_dir_all(root.join("target")).unwrap();
    fs::create_dir_all(root.join("target/nested")).unwrap();
    fs::write(root.join("target/nested/keep"), b"keep").unwrap();
    fs::create_dir_all(&mount).unwrap();
    fs::write(mount.join("source"), b"mounted").unwrap();
    let filesystem = Filesystem::new(
        root.clone(),
        vec![ResolvedMount {
            source: mount,
            target: "/data".into(),
            access: Access::ReadWrite,
        }],
    );

    assert_eq!(filesystem.stat("/data/source").unwrap().size, 7);
    let mut bytes = Vec::new();
    filesystem.archive("/data/source", &mut bytes).unwrap();
    let mut archive = tar::Archive::new(&bytes[..]);
    let mut entry = archive.entries().unwrap().next().unwrap().unwrap();
    assert_eq!(entry.path().unwrap(), FsPath::new("source"));
    let mut contents = Vec::new();
    entry.read_to_end(&mut contents).unwrap();
    assert_eq!(contents, b"mounted");

    filesystem
        .extract(
            "/target",
            &tar_with_directory("nested", "nested/file", b"copied")[..],
            Limits::default(),
        )
        .unwrap();
    assert_eq!(fs::read(root.join("target/nested/file")).unwrap(), b"copied");
    assert_eq!(fs::read(root.join("target/nested/keep")).unwrap(), b"keep");
}

#[test]
fn root_archive_contains_root_entries_without_private_snapshot_name() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("container-private-id");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("bin/tool"), b"elf").unwrap();
    let filesystem = Filesystem::new(root, Vec::new());
    let mut bytes = Vec::new();
    filesystem.archive("/", &mut bytes).unwrap();
    let paths = tar::Archive::new(&bytes[..])
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(paths.iter().any(|path| path == FsPath::new("bin/tool")));
    assert!(paths.iter().all(|path| !path.starts_with("container-private-id")));
}

#[test]
fn overlay_routes_reads_whiteouts_copy_up_writes_and_merged_archives() {
    let fixture = OverlayFixture::new();
    let filesystem = &fixture.filesystem;
    let lower = &fixture.lower;
    let upper = &fixture.upper;

    assert_eq!(filesystem.stat("/data/lower").unwrap().size, 5);
    assert_eq!(filesystem.stat("/data/copied").unwrap().size, 9);
    assert!(matches!(
        filesystem.stat("/data/deleted"),
        Err(Error::Io(ref error)) if error.kind() == std::io::ErrorKind::NotFound
    ));

    filesystem
        .extract_owned(
            "/data/sub",
            &tar_owned("written", b"copy-up", 71, 72)[..],
            Limits::default(),
            true,
        )
        .unwrap();
    assert_eq!(fs::read(upper.join("data/sub/written")).unwrap(), b"copy-up");
    assert!(!lower.join("data/sub/written").exists());
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(upper.join("data/sub/written"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );

    let mut bytes = Vec::new();
    filesystem.archive("/data", &mut bytes).unwrap();
    let paths = tar::Archive::new(&bytes[..])
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(paths.iter().any(|path| path == FsPath::new("data/lower")));
    assert!(paths.iter().any(|path| path == FsPath::new("data/copied")));
    assert!(paths.iter().any(|path| path == FsPath::new("data/sub/written")));
    assert!(!paths.iter().any(|path| path == FsPath::new("data/deleted")));

    let mut root_bytes = Vec::new();
    filesystem.archive("/", &mut root_bytes).unwrap();
    let root_paths = tar::Archive::new(&root_bytes[..])
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(root_paths.iter().any(|path| path == FsPath::new("data/lower")));
    assert!(root_paths.iter().any(|path| path == FsPath::new("data/copied")));
    assert!(!root_paths.iter().any(|path| path == FsPath::new("data/deleted")));
}

#[test]
fn overlay_preserves_ownership_and_confines_symbolic_links() {
    let fixture = OverlayFixture::new();
    let filesystem = &fixture.filesystem;
    filesystem
        .extract_owned(
            "/data/sub",
            &tar_owned("written", b"copy-up", 71, 72)[..],
            Limits::default(),
            true,
        )
        .unwrap();
    let mut bytes = Vec::new();
    filesystem.archive("/data", &mut bytes).unwrap();
    let mut archive = tar::Archive::new(&bytes[..]);
    let lower_header = archive
        .entries()
        .unwrap()
        .find_map(|entry| {
            let entry = entry.unwrap();
            (entry.path().unwrap() == FsPath::new("data/lower"))
                .then(|| (entry.header().uid().unwrap(), entry.header().gid().unwrap()))
        })
        .unwrap();
    assert_eq!(lower_header, (41, 42));
    let upper_ownership = filesystem.overlay.as_ref().unwrap().upper_ownership.lock().unwrap();
    let ownership_entries = upper_ownership
        .iter()
        .map(|(path, ownership)| (path.to_owned(), ownership))
        .collect::<Vec<_>>();
    assert_eq!(
        upper_ownership.get("data/sub/written"),
        Some(hl_images::snapshot::Ownership { uid: 71, gid: 72 }),
        "upper ownership entries: {ownership_entries:?}"
    );

    #[cfg(unix)]
    {
        let outside = fixture.temporary.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.lower.join("escape")).unwrap();
        assert!(
            filesystem
                .extract("/escape", &tar(&[("bad", b"bad")])[..], Limits::default())
                .is_err()
        );
    }
}

#[test]
fn rejects_traversal_symlink_escape_readonly_and_limits() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    let readonly = temporary.path().join("readonly");
    fs::create_dir_all(root.join("target")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&readonly).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    let filesystem = Filesystem::new(
        root,
        vec![ResolvedMount {
            source: readonly.clone(),
            target: "/readonly".into(),
            access: Access::ReadOnly,
        }],
    );

    assert!(filesystem.stat("/../outside").is_err());
    #[cfg(unix)]
    assert!(
        filesystem
            .extract("/escape", &tar(&[("owned", b"no")])[..], Limits::default())
            .is_err()
    );
    assert!(matches!(
        filesystem.extract("/readonly", &tar(&[("owned", b"no")])[..], Limits::default()),
        Err(Error::ReadOnly(_))
    ));
    assert!(
        filesystem
            .extract(
                "/target",
                &tar(&[("large", b"too large")])[..],
                Limits { entries: 1, bytes: 2 }
            )
            .is_err()
    );
    assert!(!outside.join("owned").exists());
    assert!(!readonly.join("owned").exists());
}

#[test]
fn malformed_late_entry_does_not_partially_mutate() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    fs::create_dir_all(root.join("target")).unwrap();
    let filesystem = Filesystem::new(root.clone(), Vec::new());
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        let mut file = tar::Header::new_gnu();
        file.set_size(7);
        file.set_mode(0o644);
        file.set_cksum();
        archive.append_data(&mut file, "would-exist", &b"partial"[..]).unwrap();
        let mut fifo = tar::Header::new_gnu();
        fifo.set_entry_type(tar::EntryType::Fifo);
        fifo.set_size(0);
        fifo.set_mode(0o644);
        fifo.set_cksum();
        archive.append_data(&mut fifo, "unsupported", &b""[..]).unwrap();
        archive.finish().unwrap();
    }
    assert!(filesystem.extract("/target", &bytes[..], Limits::default()).is_err());
    assert!(!root.join("target/would-exist").exists());
}

#[test]
fn replacement_kinds() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let target = root.join("target");
    fs::create_dir_all(target.join("directory")).unwrap();
    fs::write(target.join("file"), b"original").unwrap();
    fs::write(target.join("directory/keep"), b"keep").unwrap();
    let filesystem = Filesystem::new(root, Vec::new());
    let guarded = Extraction {
        no_overwrite_dir_non_dir: true,
        ..Extraction::default()
    };

    assert!(matches!(
        filesystem.extract_with(
            "/target",
            &tar_with_directory("file", "file/new", b"new")[..],
            Limits::default(),
            guarded,
        ),
        Err(Error::InvalidSpec(message)) if message.contains("cannot overwrite non-directory")
    ));
    assert_eq!(fs::read(target.join("file")).unwrap(), b"original");

    assert!(matches!(
        filesystem.extract_with(
            "/target",
            &tar(&[("directory", b"replacement")])[..],
            Limits::default(),
            guarded,
        ),
        Err(Error::InvalidSpec(message)) if message.contains("cannot overwrite directory")
    ));
    assert_eq!(fs::read(target.join("directory/keep")).unwrap(), b"keep");

    filesystem
        .extract_with("/target", &tar(&[("file", b"updated")])[..], Limits::default(), guarded)
        .unwrap();
    filesystem
        .extract_with(
            "/target",
            &tar_with_directory("directory", "directory/new", b"new")[..],
            Limits::default(),
            guarded,
        )
        .unwrap();
    assert_eq!(fs::read(target.join("file")).unwrap(), b"updated");
    assert_eq!(fs::read(target.join("directory/keep")).unwrap(), b"keep");
    assert_eq!(fs::read(target.join("directory/new")).unwrap(), b"new");
}

#[test]
fn successful_external_write_advances_the_shared_epoch() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    fs::create_dir_all(root.join("target")).unwrap();
    let generation = crate::generation::Generation::open(temporary.path().join("generation")).unwrap();
    let filesystem = Filesystem::new(root, Vec::new()).with_generation(generation.clone());
    let before = fs::read(generation.path()).unwrap();

    filesystem
        .extract("/target", &tar(&[("visible", b"now")])[..], Limits::default())
        .unwrap();

    assert_ne!(fs::read(generation.path()).unwrap(), before);
}
