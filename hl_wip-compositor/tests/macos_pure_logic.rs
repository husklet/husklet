//! Pure-logic coverage for the macOS surface presenter that needs NO Metal device / GUI session.
//!
//! The whole `surface/macos` module is gated `all(feature = "macos-surface", target_os = "macos")`
//! and its Cocoa/Metal/IOSurface paths require a real GPU + AppKit main thread — none of which exist on
//! this Linux CI host, so this file compiles to nothing here (the crate-level `cfg` makes it empty).
//! On macOS with `--features macos-surface` it exercises the one platform-neutral helper that is pure
//! arithmetic — the BGRA→RGBA channel swap the presenter uses for readback/PNG — with no device.
//!
//! Everything else in `surface/macos` (device creation, texture upload/wrap/readback, the composite
//! render pass, IOSurface lookup, NSWindow/CAMetalLayer) is only provable on a real Metal GPU and is
//! covered by `tests/macos_present_smoke.rs` + the `present_window` example. See the mission report for
//! what cannot be exercised on Linux.

#![cfg(all(feature = "macos-surface", target_os = "macos"))]

use hl_compositor::surface::macos::bgra_to_rgba;

#[test]
fn bgra_to_rgba_swaps_channels_and_forces_opaque() {
    // Two pixels: (B,G,R,A) = (10,20,30,40) and (200,150,100,0).
    let bgra = vec![10, 20, 30, 40, 200, 150, 100, 0];
    let rgba = bgra_to_rgba(&bgra);
    // R<->B swapped, G kept, alpha forced to 0xff.
    assert_eq!(rgba, vec![30, 20, 10, 0xff, 100, 150, 200, 0xff]);
}

#[test]
fn bgra_to_rgba_empty_input() {
    assert!(bgra_to_rgba(&[]).is_empty());
}
