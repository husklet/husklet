use std::{fs, path::Path};

/// The engine, not the host line discipline, owns a guest terminal's termios.
///
/// A guest that saves its termios, edits one flag and restores it must read
/// back everything it wrote. The store answers TCGETS from the guest's own
/// image whenever the host still holds the projection that installing it
/// produced, and reports a miss — today's host translation — whenever it does
/// not.
#[test]
fn a_guest_terminal_reads_back_the_termios_it_installed_on_both_isas() {
    for isa in [1, 2] {
        hl_native::terminal_termios_store_test(isa)
            .unwrap_or_else(|status| panic!("ISA {isa} terminal termios store failed at step {status}"));
    }
}

/// `termios_l2m`/`termios_m2l` translate only the flags with a BSD counterpart.
/// The bits below have none, so a macOS host cannot carry them and the guest's
/// own image is the only place they survive. This pins why the store exists: if
/// a future lane teaches the translation tables these bits, this assertion is
/// the one that should be revisited deliberately rather than silently.
#[test]
fn the_host_termios_translation_cannot_carry_every_linux_flag() {
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let compat = fs::read_to_string(native.join("linux_abi/container/netns/unix_compat.c"))
        .expect("read the host termios translation tables");
    let lflag = compat
        .split("static const uint32_t TIO_L[][2] = {")
        .nth(1)
        .and_then(|tail| tail.split("};").next())
        .expect("locate the c_lflag translation table");
    for (bit, name) in [
        ("{0x200,", "ECHOCTL"),
        ("{0x800,", "ECHOKE"),
        ("{0x400,", "ECHOPRT"),
        ("{0x10000,", "EXTPROC"),
    ] {
        assert!(
            !lflag.contains(bit),
            "{name} is now translatable; the store's guest image is no longer its only carrier"
        );
    }

    // Guest-created /dev/ptmx pairs go through fs/control.c, which has the identical round trip. A
    // master's termios is engine state with no host object behind it, so its guest image is cached
    // directly; a pts slave is a real host terminal and uses the store like any bound terminal.
    let control = fs::read_to_string(native.join("linux_abi/syscall/fs/control.c"))
        .expect("read the ptmx terminal ioctl routing");
    assert_eq!(
        control.matches("memcpy(arg, g_ptm_image[fd], 36)").count(),
        2,
        "TCGETS and TCGETS2 on a pty master must answer from the guest's own image"
    );
    assert_eq!(
        control.matches("memcpy(g_ptm_image[fd], arg, 36)").count(),
        2,
        "TCSETS and TCSETS2 on a pty master must record the guest's own image"
    );
    assert_eq!(
        control
            .matches("terminal_termios_apply_recall(tfd, (uint8_t *)arg)")
            .count(),
        2,
        "TCGETS and TCGETS2 on a pts slave must answer from the engine-owned image"
    );
    assert_eq!(
        control
            .matches("terminal_termios_observe_set(tfd, (const uint8_t *)arg)")
            .count(),
        2,
        "TCSETS and TCSETS2 on a pts slave must record the image the guest installed"
    );

    let route = fs::read_to_string(native.join("linux_abi/syscall/binding/route_bound.c"))
        .expect("read the bound terminal ioctl routing");
    // Both getters and both setters must be wired. Counting rather than
    // matching once is deliberate: TCGETS and TCGETS2 call the same helper, so
    // a single `contains` stays green when either one loses its call.
    assert_eq!(
        route
            .matches("terminal_termios_apply_recall(native_fd, argument)")
            .count(),
        2,
        "TCGETS and TCGETS2 must both answer from the engine-owned image"
    );
    assert_eq!(
        route
            .matches("terminal_termios_observe_set(native_fd, argument)")
            .count(),
        2,
        "TCSETS{{,W,F}} and TCSETS2{{,W2,F2}} must both record the image the guest installed"
    );
}
