//! Manual bring-up harness: run the compositor on the STANDARD Wayland discovery socket so a REAL,
//! third-party client can be launched against it by hand and its composited frames inspected as PNGs.
//!
//! This is the interactive companion to `tests/wayland_live_socket.rs` (which drives an in-process
//! `wayland-client`): it proves an *external* GUI binary — weston-terminal, weston-simple-egl, a GTK app,
//! Chrome, ... — can discover this compositor exactly the way it discovers any real one, via
//! `$WAYLAND_DISPLAY`. No DRM, no display, no GPU: the `PngPresenter` is the output.
//!
//! Usage:
//! ```text
//! export XDG_RUNTIME_DIR=$(mktemp -d); chmod 700 "$XDG_RUNTIME_DIR"
//! export HL_PROBE_PNG="$XDG_RUNTIME_DIR/png"        # where composited frames are written
//! cargo run --features smithay-adapter --example serve_auto_probe &
//! # it prints e.g. `WAYLAND_DISPLAY=wayland-1`
//! export WAYLAND_DISPLAY=wayland-1
//! weston-terminal            # or weston-simple-egl, or any real Wayland client
//! ls "$HL_PROBE_PNG"         # the client's committed frames, composited to disk
//! ```

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use hl_compositor::adapter::smithay::{self, PngPresenter};

fn main() {
    // Composited frames are written here as PNGs (default under /tmp) so an external client's output can
    // be inspected without a display.
    let png_dir = std::env::var("HL_PROBE_PNG").unwrap_or_else(|_| "/tmp/hl-probe-png".into());
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(png_dir);
    smithay::run_auto(presenter, stop, |name| {
        // Publish the socket the client should point `$WAYLAND_DISPLAY` at.
        println!("WAYLAND_DISPLAY={}", name.to_string_lossy());
    })
    .expect("serve loop");
}
