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
    let source =
        fs::read_to_string(native.join("syscall/binding/route_bound.c")).expect("bound syscall routing source");
    let close = source
        .split("case 57: /* close */")
        .nth(1)
        .and_then(|body| body.split("case 62:").next())
        .expect("bound close implementation");

    let reset = close
        .find("fd_reset_emul((int)source.fd);")
        .expect("bound close must clear overlay directory cursors and all other fd-indexed emulation state");
    let provider_close = close
        .find("hl_linux_close(g_linux_box, source.fd)")
        .expect("provider close");
    assert!(
        reset < provider_close,
        "fd emulation state must be released while the underlying descriptor remains live"
    );
}

#[test]
fn forwarded_filesystem_calls_carry_the_workers_current_dac_credentials() {
    let native = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi");
    let mailbox = fs::read_to_string(native.join("sentry.c")).expect("sentry mailbox source");
    let route = fs::read_to_string(native.join("sentry_route.c")).expect("sentry route source");
    let service = fs::read_to_string(native.join("sentry_service.c")).expect("sentry service source");
    let state = fs::read_to_string(native.join("container/state.c")).expect("credential state source");

    assert!(mailbox.contains("hl_sentry_credential_snapshot credentials;"));
    assert!(route.contains("R->credentials.fsuid = credentials.fsuid;"));
    assert!(route.contains("R->credentials.capabilities = credentials.capabilities;"));
    assert!(service.contains("g_sentry_credentials_override = &R->credentials;"));
    assert!(state.contains("credentials.fsuid = g_sentry_credentials_override->fsuid;"));
    assert!(state.contains("return (int)g_sentry_credentials_override->fsuid;"));
}
#[test]
fn bound_file_mutations_evict_cached_path_metadata() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi/syscall/binding");
    let descriptor = std::fs::read_to_string(root.join("descriptor.c")).unwrap();
    let poll = std::fs::read_to_string(root.join("poll.c")).unwrap();
    let route = std::fs::read_to_string(root.join("route_bound.c")).unwrap();

    assert!(descriptor.contains("if (!output && result > 0) bound_evict_handle(file->host_handle);"));
    assert!(poll.contains("if (result > 0) bound_evict_handle(file->host_handle);"));
    assert!(route.contains("if (result == 0) bound_evict_handle(target);"));
}
#[test]
fn copy_file_range_preserves_both_descriptor_authorities() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/linux_abi");
    let sentry = std::fs::read_to_string(root.join("sentry_service.c")).unwrap();
    let route = std::fs::read_to_string(root.join("syscall/binding/route_special.c")).unwrap();
    let transfer = std::fs::read_to_string(root.join("syscall/binding/poll.c")).unwrap();

    assert!(sentry.contains("g_bound_source_native = !p->typed[input];"));
    assert!(sentry.contains("g_bound_second_native = !p->typed[output];"));
    assert!(route.contains("bound_copy_file_range("));
    assert!(transfer.contains("if (done != 0 && output != NULL) bound_evict_handle(output->host_handle);"));
}
