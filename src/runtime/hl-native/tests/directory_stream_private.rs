#![cfg(feature = "native-test-hooks")]

//! The macOS host serves a handle's directory listing through a DIR* opened on its own duplicate of
//! the handle's descriptor. That duplicate belongs to the engine, so it must live in the
//! engine-private band with a ledger row: `exec_fd_is_engine()` decides the guest's
//! `close_range(3, ~0U)` from that ledger, and `engine_fd_vacate()` -- which does not enumerate these
//! streams -- cannot protect a number the guest `dup2()`s onto. A stream left on the lowest free
//! number is closed by an ordinary guest and the cached DIR* then ends the listing early or fails
//! EINVAL, with nothing in the guest's view to explain it.
//!
//! This is a macOS host-adapter contract and there is nothing here for another host to run: the
//! `hl_native::directory_stream_private_test` hook itself does not exist off Darwin. The file used
//! to gate on `target_os = "macos"` at file scope, so on a Linux host it compiled to zero tests and
//! reported `test result: ok`. It now names the host it left uncovered instead, which matters
//! because 28da76945 and c6ab34d1e touched exactly this mechanism from a Linux box.

#[cfg(target_os = "macos")]
fn verdict(scenario: u32) -> i32 {
    hl_native::directory_stream_private_test(scenario)
}

#[cfg(target_os = "macos")]
#[test]
fn directory_stream_descriptor_is_engine_private() {
    // 1: no stream opened (bad fixture); 2: no ledger row; 3: still inside the guest's band.
    assert_eq!(verdict(0), 0, "directory stream is not an engine-private descriptor");
}

#[cfg(target_os = "macos")]
#[test]
fn closing_a_directory_handle_retires_its_stream_row() {
    // 4: the private-ledger population grew across 64 open/list/close rounds.
    assert_eq!(verdict(1), 0, "directory-handle close leaked private-ledger rows");
}

/// A file gated out at file scope is indistinguishable in the harness output from one whose tests
/// all passed, so say which coverage this host does not have. The notice goes to the real stderr
/// descriptor rather than through `eprintln!`, because libtest captures Rust-level output and
/// prints it only for a FAILING test -- the same reason `hl-native`'s `guest_compiler_present`
/// skip notice writes to descriptor 2.
#[cfg(not(target_os = "macos"))]
#[test]
fn engine_private_directory_streams_are_uncovered_on_this_host() {
    let notice = "SKIP directory_stream_private: 2 cases left UNCOVERED -- the macOS host adapter's \
                  DIR* descriptor band exists only on Darwin.\n";
    // SAFETY: a write of a `'static` initialized buffer to the process's stderr descriptor. It
    // borrows nothing beyond the call, and a short or failed write is not an error worth acting on.
    #[allow(unsafe_code)]
    unsafe {
        libc::write(2, notice.as_ptr().cast(), notice.len());
    }
}
