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

#[test]
fn bound_close_clears_fd_emulation_state_before_closing_the_provider_handle() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi");
    let source = fs::read_to_string(native.join("syscall/binding/route_bound.c"))
        .expect("bound syscall routing source");
    let close = source
        .split("case 57: /* close */")
        .nth(1)
        .and_then(|body| body.split("case 62:").next())
        .expect("bound close implementation");

    let reset = close.find("fd_reset_emul((int)source.fd);").expect(
        "bound close must clear overlay directory cursors and all other fd-indexed emulation state",
    );
    let provider_close = close
        .find("hl_linux_close(g_linux_box, source.fd)")
        .expect("provider close");
    assert!(
        reset < provider_close,
        "fd emulation state must be released while the underlying descriptor remains live"
    );
}
