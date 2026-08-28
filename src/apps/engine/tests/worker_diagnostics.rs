//! What a developer sees on descriptor 1 and descriptor 2 when a worker refuses.
//!
//! These assert on the *bytes the process writes*, not on its exit code. An exit-code assertion
//! passes unchanged against the defect this file exists for: every one of these commands already
//! exited 2 or 125, and every one of them printed nothing at all, because
//! `try_parse_from(..).unwrap_or_else(|_| exit(2))` discarded clap's message and
//! `backend_receipt` answered `Err(())`.

use std::process::{Command, Output};

fn worker(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hl-x86_64"))
        .args(arguments)
        .output()
        .expect("run the x86-64 worker")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn absent_rootfs() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("hl-absent-rootfs-{}", std::process::id()))
}

#[test]
fn a_mistyped_flag_is_named_and_a_near_match_is_offered() {
    let output = worker(&["--rootfsx", "/tmp", "bin/sh"]);
    let text = stderr(&output);
    assert!(text.contains("unexpected argument '--rootfsx' found"), "{text}");
    assert!(text.contains("a similar argument exists: '--rootfs'"), "{text}");
    assert!(text.contains("Usage: hl-x86_64"), "{text}");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_missing_operand_is_named() {
    let output = worker(&[]);
    let text = stderr(&output);
    assert!(
        text.contains("the following required arguments were not provided"),
        "{text}"
    );
    assert!(text.contains("<EXECUTABLE>"), "{text}");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn help_and_version_are_answered_on_standard_output() {
    let help = worker(&["--help"]);
    let text = stdout(&help);
    assert!(text.contains("Usage: hl-x86_64"), "{text}");
    assert!(text.contains("--rootfs"), "{text}");
    assert!(text.contains("--translit-jcc-ibtc <on|off>"), "{text}");
    assert_eq!(help.status.code(), Some(0));

    let version = worker(&["--version"]);
    assert!(stdout(&version).starts_with("hl-x86_64 "), "{}", stdout(&version));
    assert_eq!(version.status.code(), Some(0));
}

#[test]
fn a_rootfs_that_does_not_exist_is_named() {
    let absent = absent_rootfs();
    let output = worker(&["--rootfs", absent.to_str().unwrap(), "bin/sh"]);
    assert_eq!(
        stderr(&output).trim_end(),
        format!(
            "hl-x86_64: the rootfs {} is not an existing directory",
            absent.display()
        )
    );
    assert_eq!(output.status.code(), Some(125));
}

#[test]
fn an_entry_the_image_does_not_carry_is_named() {
    let root = tempfile::tempdir().expect("temporary rootfs");
    let output = worker(&["--rootfs", root.path().to_str().unwrap(), "bin/nope"]);
    assert_eq!(
        stderr(&output).trim_end(),
        format!(
            "hl-x86_64: the rootfs {} has no executable file at the guest path /bin/nope",
            root.path().display()
        )
    );
    assert_eq!(output.status.code(), Some(125));
}

#[test]
fn a_worker_refuses_the_other_guest_isa_out_loud() {
    let output = worker(&["--guest-isa", "aarch64", "bin/sh"]);
    assert_eq!(
        stderr(&output).trim_end(),
        "hl-x86_64: this worker runs x86_64 guests, so it cannot serve --guest-isa aarch64"
    );
    assert_eq!(output.status.code(), Some(125));
}

#[test]
fn a_receipt_that_cannot_be_produced_says_why() {
    let output = worker(&["--backend-receipt", "--guest-isa", "x86_64"]);
    assert_eq!(
        stderr(&output).trim_end(),
        "hl-x86_64: this worker already fixes the guest ISA to x86_64, so --guest-isa cannot select another"
    );
    assert_eq!(output.status.code(), Some(125));
}

/// An engine refusal is announced with its reason, the same as a refusal this worker makes itself.
///
/// This case does **not** prove the absolute symlink was resolved inside the image -- measured: it
/// stays green when the resolver is replaced by `rootfs.join(entry)`, because both spellings end
/// in an engine refusal and the sentence is the same. `retained_direct_linux`'s
/// `an_absolute_guest_symlink_resolves_to_the_image_copy` runs a real guest and reads its exit
/// status, which is what tells the two apart.
#[cfg(unix)]
#[test]
fn an_engine_refusal_is_announced_with_its_reason() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let root = tempfile::tempdir().expect("temporary rootfs");
    let busybox = root.path().join("bin/busybox");
    std::fs::create_dir_all(busybox.parent().unwrap()).unwrap();
    // Not a loadable image -- the point is which host path the worker resolves to and opens, which
    // it reports before the loader ever looks at the bytes.
    std::fs::write(&busybox, b"\x7fELF not really").unwrap();
    std::fs::set_permissions(&busybox, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("/bin/busybox", root.path().join("bin/sh")).unwrap();

    let output = worker(&["--rootfs", root.path().to_str().unwrap(), "bin/sh"]);
    let text = stderr(&output);
    // Resolution succeeded, so the refusal comes from the engine and not from this worker's own
    // "no executable file at the guest path" arm.
    assert!(text.starts_with("hl-x86_64: the engine refused this launch:"), "{text}");
    assert!(
        !text.contains("has no executable file"),
        "the absolute symlink was not resolved inside the rootfs: {text}"
    );
}
