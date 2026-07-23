use super::super::copy::{Archive, Copy};
use hl_images::snapshot::{Ownership, Ownerships};

#[test]
fn archive_detection_distinguishes_gzip_tar_and_plain_files() {
    use std::io::Write as _;

    let root = tempfile::tempdir().unwrap();
    let gzip = root.path().join("payload.tar.gz");
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&gzip).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(b"payload").unwrap();
    encoder.finish().unwrap();

    let tar_path = root.path().join("payload.tar");
    let mut tar = tar::Builder::new(std::fs::File::create(&tar_path).unwrap());
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "payload", &b"payload"[..])
        .unwrap();
    tar.finish().unwrap();

    let plain = root.path().join("plain");
    std::fs::write(&plain, b"payload").unwrap();
    let empty_tar = root.path().join("empty.tar");
    tar::Builder::new(std::fs::File::create(&empty_tar).unwrap())
        .finish()
        .unwrap();
    let corrupt_tar = root.path().join("corrupt.tar");
    let mut corrupt = std::fs::read(&tar_path).unwrap();
    corrupt[148] ^= 0xff;
    std::fs::write(&corrupt_tar, corrupt).unwrap();
    let short_zeros = root.path().join("short-zeros");
    std::fs::write(&short_zeros, [0_u8; 512]).unwrap();
    assert_eq!(Archive::detect(&gzip).unwrap(), Some(Archive::Gzip));
    assert_eq!(Archive::detect(&tar_path).unwrap(), Some(Archive::Tar));
    assert_eq!(Archive::detect(&empty_tar).unwrap(), Some(Archive::Tar));
    assert_eq!(Archive::detect(&plain).unwrap(), None);
    assert_eq!(Archive::detect(&corrupt_tar).unwrap(), None);
    assert_eq!(Archive::detect(&short_zeros).unwrap(), None);
}

#[test]
fn copy_excludes_and_parents_apply_docker_default_root_ownership() {
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(source.path().join("tree/private")).unwrap();
    std::fs::write(source.path().join("tree/keep"), b"keep").unwrap();
    std::fs::write(source.path().join("tree/drop.tmp"), b"drop").unwrap();
    std::fs::write(source.path().join("tree/private/hidden"), b"hidden").unwrap();
    let destination = tempfile::tempdir().unwrap();
    let mut ownerships = Ownerships::memory();
    Copy {
        source: source.path(),
        sources: &["tree".into()],
        target: "filtered/",
        directory: "/",
        destination: destination.path(),
        unpack: false,
        mode: None,
        owner: None,
        excludes: &["*.tmp".into(), "private".into()],
        parents: false,
    }
    .apply(&mut ownerships)
    .unwrap();
    assert!(destination.path().join("filtered/keep").exists());
    assert!(!destination.path().join("filtered/drop.tmp").exists());
    assert!(!destination.path().join("filtered/private").exists());
    assert_eq!(
        ownerships.get("filtered/keep"),
        Some(Ownership { uid: 0, gid: 0 })
    );
    assert_eq!(ownerships.get("filtered/drop.tmp"), None);

    Copy {
        source: source.path(),
        sources: &["./tree/keep".into()],
        target: "parents/",
        directory: "/",
        destination: destination.path(),
        unpack: false,
        mode: Some(0o600),
        owner: None,
        excludes: &[],
        parents: true,
    }
    .apply(&mut ownerships)
    .unwrap();
    assert!(destination.path().join("parents/tree/keep").exists());
    assert_eq!(
        ownerships.get("parents/tree/keep"),
        Some(Ownership { uid: 0, gid: 0 })
    );
}

#[cfg(unix)]
#[test]
fn copy_mode_is_applied_and_symlinked_destination_is_replaced() {
    use std::os::unix::fs::PermissionsExt as _;
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("tool"), "inside").unwrap();
    let destination = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), destination.path().join("tool")).unwrap();
    Copy {
        source: source.path(),
        sources: &["tool".into()],
        target: "/tool",
        directory: "/",
        destination: destination.path(),
        unpack: false,
        mode: Some(0o750),
        owner: None,
        excludes: &[],
        parents: false,
    }
    .apply(&mut Ownerships::memory())
    .unwrap();
    assert_eq!(std::fs::read_to_string(outside.path()).unwrap(), "outside");
    assert_eq!(
        std::fs::read_to_string(destination.path().join("tool")).unwrap(),
        "inside"
    );
    assert_eq!(
        std::fs::metadata(destination.path().join("tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
}

#[test]
fn add_unpacks_local_tar_while_copy_preserves_it() {
    let source = tempfile::tempdir().unwrap();
    let archive = source.path().join("payload.tar");
    {
        let file = std::fs::File::create(&archive).unwrap();
        let mut tar = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "inside", &b"payload"[..])
            .unwrap();
        tar.finish().unwrap();
    }
    for (unpack, target) in [(false, "/copied/"), (true, "/added/")] {
        let destination = tempfile::tempdir().unwrap();
        Copy {
            source: source.path(),
            sources: &["payload.tar".into()],
            target,
            directory: "/",
            destination: destination.path(),
            unpack,
            mode: None,
            owner: None,
            excludes: &[],
            parents: false,
        }
        .apply(&mut Ownerships::memory())
        .unwrap();
        if unpack {
            assert_eq!(
                std::fs::read_to_string(destination.path().join("added/inside")).unwrap(),
                "payload"
            );
            assert!(!destination.path().join("added/payload.tar").exists());
        } else {
            assert!(destination.path().join("copied/payload.tar").is_file());
            assert!(!destination.path().join("copied/inside").exists());
        }
    }
}
