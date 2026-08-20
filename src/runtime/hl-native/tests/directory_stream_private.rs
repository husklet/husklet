#![cfg(all(feature = "native-test-hooks", target_os = "macos"))]

//! The macOS host serves a handle's directory listing through a DIR* opened on its own duplicate of
//! the handle's descriptor. That duplicate belongs to the engine, so it must live in the
//! engine-private band with a ledger row: `exec_fd_is_engine()` decides the guest's
//! `close_range(3, ~0U)` from that ledger, and `engine_fd_vacate()` -- which does not enumerate these
//! streams -- cannot protect a number the guest `dup2()`s onto. A stream left on the lowest free
//! number is closed by an ordinary guest and the cached DIR* then ends the listing early or fails
//! EINVAL, with nothing in the guest's view to explain it.

fn verdict(scenario: u32) -> i32 {
    hl_native::directory_stream_private_test(scenario)
}

#[test]
fn directory_stream_descriptor_is_engine_private() {
    // 1: no stream opened (bad fixture); 2: no ledger row; 3: still inside the guest's band.
    assert_eq!(verdict(0), 0, "directory stream is not an engine-private descriptor");
}

#[test]
fn closing_a_directory_handle_retires_its_stream_row() {
    // 4: the private-ledger population grew across 64 open/list/close rounds.
    assert_eq!(verdict(1), 0, "directory-handle close leaked private-ledger rows");
}
