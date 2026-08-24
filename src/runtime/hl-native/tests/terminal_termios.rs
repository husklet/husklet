#[cfg(unix)]
use std::os::fd::AsRawFd;
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
            .matches(
                "terminal_termios_observe_set(tfd, (const uint8_t *)arg, terminal_termios_flush_request(rq))",
            )
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
            .matches(
                "terminal_termios_observe_set(native_fd, argument, terminal_termios_flush_request(request))",
            )
            .count(),
        2,
        "TCSETS{{,W,F}} and TCSETS2{{,W2,F2}} must both record the image the guest installed"
    );
}

/// The engine's terminal pump reaches the guest's termios through the bridge table, not through the
/// host terminal, because once the pump puts the host slave in raw mode the host no longer carries
/// what the guest asked for.
///
/// The store is keyed by terminal identity via `fstat`, so a pipe stands in for a terminal and this
/// runs without a pty on any host. The C-side store test has already installed images by the time
/// this runs in the same binary, so the generation is expected to be non-zero and to move.
///
/// Unix only, because `hl_native::terminal_termios` is: the lib gates it `#[cfg(unix)]` and says why
/// -- it takes a `RawFd`, and a Windows process HANDLE is not one. This case reads that function and
/// takes the raw descriptor of a `PipeReader` to call it, so the platform is its subject and the gate
/// belongs here rather than on the two cases above, which read the engine's own C sources and its
/// ISA-keyed store and run on every host.
#[cfg(unix)]
#[test]
fn the_bridge_answers_a_terminal_image_and_a_generation_that_only_moves_on_an_install() {
    let before = hl_native::terminal_termios_generation();
    for isa in [1, 2] {
        hl_native::terminal_termios_store_test(isa).expect("install images through the store");
    }
    let after = hl_native::terminal_termios_generation();
    assert!(
        after > before,
        "installing an image must advance the generation ({before} -> {after})"
    );
    // Reading it again without installing anything must not move it: that invariant is what lets a
    // per-keystroke path skip the lookup.
    assert_eq!(
        after,
        hl_native::terminal_termios_generation(),
        "the generation moved without an install"
    );

    // An unconfigured terminal reports nothing and leaves the buffer alone.
    let mut image = [0xab_u8; 36];
    let (reader, _writer) = std::io::pipe().expect("create a probe pipe");
    assert!(
        hl_native::terminal_termios(reader.as_raw_fd(), &mut image).is_none(),
        "an unconfigured terminal must not answer with someone else's image"
    );
    assert_eq!(image, [0xab_u8; 36], "a miss must leave the buffer untouched");

    // Install a distinctive image and read it back through the bridge. This is the path a pump
    // takes, and it carries the flags a raw host slave cannot: ECHOCTL and ECHOKE in c_lflag, and
    // ICANON, which is the one the discipline turns on the guest's behalf.
    let mut installed = [0_u8; 36];
    installed[12] = 0x3b; // ISIG|ICANON|ECHO|ECHOE|ECHOK
    installed[13] = 0x0a; // ECHOCTL 0x200 | ECHOKE 0x800
    installed[17 + 4] = 0x04; // c_cc[VEOF]
    installed[17 + 2] = 0x7f; // c_cc[VERASE]
    hl_native::terminal_termios_install_test(reader.as_raw_fd(), &installed);
    let mut read_back = [0_u8; 36];
    assert!(
        hl_native::terminal_termios(reader.as_raw_fd(), &mut read_back).is_some(),
        "the bridge must find the image that was just installed"
    );
    assert_eq!(
        read_back, installed,
        "the bridge must answer with the guest's own image, byte for byte"
    );

    // And installing advanced the generation, so a pump watching it learns to re-read.
    assert!(
        hl_native::terminal_termios_generation() > after,
        "installing an image must advance the generation a pump watches"
    );
}

/// The case above is the only one here that needs a Unix descriptor, so on a host without one this
/// target still runs its other two and would otherwise report a smaller, silent count. The notice goes
/// to the real stderr descriptor rather than through `eprintln!`, because libtest captures Rust-level
/// output and prints it only for a FAILING test.
#[cfg(not(unix))]
#[test]
fn the_terminal_termios_bridge_read_back_is_uncovered_on_this_host() {
    let notice = "SKIP terminal_termios: 1 case left UNCOVERED -- reading an installed image back \
                  through `hl_native::terminal_termios` needs a RawFd, which this host has not got.\n";
    // The CRT's _write takes its count as an unsigned int, while POSIX write takes a size_t, so the
    // length is converted at the call rather than the type being assumed.
    #[cfg(windows)]
    let count = notice.len() as libc::c_uint;
    #[cfg(not(windows))]
    let count = notice.len();
    // SAFETY: a write of a `'static` initialized buffer to the process's stderr descriptor. It borrows
    // nothing beyond the call, and a short or failed write is not an error worth acting on.
    #[allow(unsafe_code)]
    unsafe {
        libc::write(2, notice.as_ptr().cast(), count);
    }
}
