use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::*;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let path = std::env::temp_dir().join(format!(
                "hl-fs-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create test directory {}: {error}", path.display()),
            }
        }
        panic!("temporary test directory names exhausted");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn bounded_read_limit() {
    let temporary = TemporaryDirectory::new("bounded");
    let path = temporary.path().join("data");
    fs::write(&path, b"abcd").unwrap();
    assert_eq!(BoundedFile::read(&path, 4).unwrap(), b"abcd");
    assert!(matches!(
        BoundedFile::read(&path, 3),
        Err(FsError::LimitExceeded { limit: 3, .. })
    ));
    assert_eq!(
        BoundedFile::read(&path, 0).unwrap_err().to_string(),
        format!("{} exceeds the 0-byte limit", path.display())
    );
}

#[test]
fn replacement_publication() {
    let temporary = TemporaryDirectory::new("replace");
    let path = temporary.path().join("state");
    fs::write(&path, b"old").unwrap();
    AtomicFile::replace(&path, b"complete replacement", Durability::File).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"complete replacement");
    assert_eq!(Directory::new(temporary.path()).entries().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn replacement_durability() {
    let temporary = TemporaryDirectory::new("durable");
    let path = temporary.path().join("state");
    AtomicFile::replace(&path, b"durable", Durability::FileAndDirectory).unwrap();
    assert_eq!(fs::read(path).unwrap(), b"durable");
}

#[test]
fn directory_enumeration() {
    let temporary = TemporaryDirectory::new("directory");
    fs::write(temporary.path().join("zeta"), []).unwrap();
    fs::create_dir(temporary.path().join("alpha")).unwrap();
    fs::write(temporary.path().join("middle"), []).unwrap();
    let entries = Directory::new(temporary.path()).entries().unwrap();
    let names = entries
        .iter()
        .map(|entry| entry.name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, ["alpha", "middle", "zeta"]);
    assert_eq!(entries[0].kind(), EntryKind::Directory);
    assert_eq!(entries[1].kind(), EntryKind::File);
}

#[test]
fn identity_stability() {
    let temporary = TemporaryDirectory::new("identity");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::write(&first, b"a").unwrap();
    fs::write(&second, b"b").unwrap();
    assert_eq!(FileIdentity::read(&first).unwrap(), FileIdentity::read(&first).unwrap());
    assert_ne!(
        FileIdentity::read(&first).unwrap(),
        FileIdentity::read(&second).unwrap()
    );
}

#[test]
fn rejects_lexical_escape() {
    let temporary = TemporaryDirectory::new("root-shape");
    let root = Root::open(temporary.path()).unwrap();
    assert!(matches!(root.resolve("../outside"), Err(FsError::PathEscape(_))));
    assert!(matches!(root.resolve(temporary.path()), Err(FsError::PathEscape(_))));
    assert_eq!(
        root.resolve("missing/child").unwrap(),
        temporary.path().join("missing/child")
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root_directory = TemporaryDirectory::new("root-link");
    let outside = TemporaryDirectory::new("outside");
    symlink(outside.path(), root_directory.path().join("escape")).unwrap();
    let root = Root::open(root_directory.path()).unwrap();
    assert!(matches!(root.resolve("escape/child"), Err(FsError::SymlinkEscape(_))));
}

#[test]
fn existing_requires_object() {
    let temporary = TemporaryDirectory::new("root-existing");
    fs::write(temporary.path().join("present"), b"x").unwrap();
    let root = Root::open(temporary.path()).unwrap();
    assert_eq!(
        root.resolve_existing("present").unwrap(),
        temporary.path().join("present")
    );
    assert!(root.resolve_existing("missing").is_err());
}
