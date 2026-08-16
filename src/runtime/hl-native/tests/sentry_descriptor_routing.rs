use std::{fs, path::PathBuf};

#[test]
fn single_descriptor_syscalls_translate_guest_fds_at_the_sentry_boundary() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi");
    let source = fs::read_to_string(native.join("sentry_service.c")).expect("sentry service source");
    let forwarded = fs::read_to_string(native.join("forwarded.h")).expect("sentry forwarding contract");
    let start = source.find("static int fd_in_a0").expect("fd classifier");
    let end = source[start..]
        .find("// ------------------------------------------------------------------ sentry process body")
        .map(|offset| start + offset)
        .expect("end of fd classifier");
    let classifier = &source[start..end];

    for (number, operation) in [
        (32, "flock"),
        (52, "fchmod"),
        (55, "fchown"),
        (82, "fsync"),
        (83, "fdatasync"),
        (84, "sync_file_range"),
        (267, "syncfs"),
    ] {
        assert!(
            classifier.contains(&format!("case {number}:")),
            "{operation} does not translate its guest descriptor before sentry dispatch"
        );
        let case = format!("X({number})");
        assert_eq!(
            forwarded.matches(&case).count(),
            2,
            "{operation} must be both forwarded and admitted as a scalar sentry request"
        );
    }
}
