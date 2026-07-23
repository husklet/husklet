use super::super::context::{Context, Pattern};

fn context_digest(root: &std::path::Path) -> [u8; 32] {
    Context::new(root).digest().unwrap()
}

#[cfg(unix)]
#[test]
fn build_context_digest_includes_file_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("tool");
    std::fs::write(&file, "tool").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let before = context_digest(root.path());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(before, context_digest(root.path()));
}

#[cfg(unix)]
#[test]
fn build_context_digest_includes_hardlink_topology() {
    let linked = tempfile::tempdir().unwrap();
    std::fs::write(linked.path().join("a"), "same").unwrap();
    std::fs::hard_link(linked.path().join("a"), linked.path().join("b")).unwrap();
    let separate = tempfile::tempdir().unwrap();
    std::fs::write(separate.path().join("a"), "same").unwrap();
    std::fs::write(separate.path().join("b"), "same").unwrap();
    assert_ne!(
        context_digest(linked.path()),
        context_digest(separate.path())
    );
}

#[cfg(unix)]
#[test]
fn build_context_digest_includes_symlink_target() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("aa", first.path().join("link")).unwrap();
    std::os::unix::fs::symlink("bb", second.path().join("link")).unwrap();
    assert_ne!(context_digest(first.path()), context_digest(second.path()));
}

#[test]
fn build_context_digest_handles_apostrophes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("can't stop")).unwrap();
    std::fs::write(root.path().join("can't stop/O'Brien"), "content").unwrap();
    assert_eq!(context_digest(root.path()), context_digest(root.path()));
}

#[cfg(unix)]
#[test]
fn copy_source_resolution_rejects_parent_and_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    let context = Context::new(root.path());
    assert!(context.source("../outside").is_err());
    assert!(context.source("escape").is_err());
    assert_eq!(
        context.source(".").unwrap(),
        root.path().canonicalize().unwrap()
    );
}

#[test]
fn build_context_digest_is_order_independent_and_content_sensitive() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::create_dir(first.path().join("directory")).unwrap();
    std::fs::write(first.path().join("directory/file"), "same").unwrap();
    std::fs::write(first.path().join("top"), "same").unwrap();
    std::fs::write(second.path().join("top"), "same").unwrap();
    std::fs::create_dir(second.path().join("directory")).unwrap();
    std::fs::write(second.path().join("directory/file"), "same").unwrap();
    assert_eq!(context_digest(first.path()), context_digest(second.path()));
    std::fs::write(second.path().join("directory/file"), "changed").unwrap();
    assert_ne!(context_digest(first.path()), context_digest(second.path()));
}

#[test]
fn dockerignore_ordered_double_star_negation_and_cache_digest() {
    fn context(ignored: &[u8]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (path, bytes) in [
            ("Dockerfile", b"FROM scratch\n".as_slice()),
            (
                ".dockerignore",
                b"**/*.log\n!important/**/*.log\nvendor\n!vendor/keep/file\ntemp?\nfile[0-9].tmp\n\\#literal\n",
            ),
            ("visible", b"same"),
            ("root.log", ignored),
            ("nested/drop.log", ignored),
            ("important/deep/keep.log", b"keep"),
            ("vendor/drop", ignored),
            ("vendor/keep/file", b"keep"),
            ("temp1", ignored),
            ("file1.tmp", ignored),
            ("filea.tmp", b"keep"),
            ("#literal", ignored),
        ] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        root
    }

    let first = context(b"ignored-one");
    let second = context(b"ignored-two");
    let first_context = Context::new(first.path());
    let second_context = Context::new(second.path());
    first_context.ignore("Dockerfile").unwrap();
    second_context.ignore("Dockerfile").unwrap();
    for removed in [
        "Dockerfile",
        ".dockerignore",
        "root.log",
        "nested/drop.log",
        "vendor/drop",
        "temp1",
        "file1.tmp",
        "#literal",
    ] {
        assert!(!first.path().join(removed).exists(), "retained {removed}");
    }
    for retained in [
        "visible",
        "important/deep/keep.log",
        "vendor/keep/file",
        "filea.tmp",
    ] {
        assert!(first.path().join(retained).exists(), "removed {retained}");
    }
    assert_eq!(
        first_context.digest().unwrap(),
        second_context.digest().unwrap()
    );
}

#[test]
fn dockerignore_pattern_matching_is_bounded_and_segment_aware() {
    assert!(Pattern::new("src/**/test[0-9].rs").matches("src/a/b/test7.rs"));
    assert!(!Pattern::new("src/**/test[!0-9].rs").matches("src/a/test7.rs"));
    assert!(Pattern::new("*.log").matches("nested/deep/error.log"));
    assert!(!Pattern::new("src/*.rs").matches("other/src/main.rs"));
    assert!(Pattern::new(r"literal\*name").matches("literal*name"));

    let pattern = format!("{}z", "*a".repeat(256));
    let value = format!("{}z", "a".repeat(256));
    assert!(Pattern::new(&pattern).matches(&value));
}

#[cfg(unix)]
#[test]
fn build_context_dockerfile_symlink_cannot_escape_context() {
    let context = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "FROM scratch\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), context.path().join("Dockerfile")).unwrap();
    assert!(Context::new(context.path()).read("Dockerfile").is_err());
}
