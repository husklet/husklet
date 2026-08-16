use std::{fs, path::PathBuf};

#[test]
fn single_descriptor_syscalls_translate_guest_fds_at_the_sentry_boundary() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi/sentry_service.c"),
    )
    .expect("sentry service source");
    let start = source.find("static int fd_in_a0").expect("fd classifier");
    let end = source[start..]
        .find("// ------------------------------------------------------------------ sentry process body")
        .map(|offset| start + offset)
        .expect("end of fd classifier");
    let classifier = &source[start..end];

    for (number, operation) in [
        (7, "fsetxattr"),
        (10, "fgetxattr"),
        (13, "flistxattr"),
        (16, "fremovexattr"),
        (32, "flock"),
        (44, "fstatfs"),
        (52, "fchmod"),
        (55, "fchown"),
        (69, "preadv"),
        (70, "pwritev"),
        (82, "fsync"),
        (83, "fdatasync"),
        (84, "sync_file_range"),
        (213, "recvmmsg"),
        (267, "syncfs"),
        (286, "preadv2"),
        (287, "pwritev2"),
    ] {
        assert!(
            classifier.contains(&format!("case {number}:")),
            "{operation} does not translate its guest descriptor before sentry dispatch"
        );
    }
}
