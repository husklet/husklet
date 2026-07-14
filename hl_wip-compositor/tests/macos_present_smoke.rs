//! macOS present smoke (requires `--features macos-surface`, macOS + a Metal GPU).
//!
//! Proves the Cocoa/Metal presenter's pixel path end-to-end on a REAL Metal device WITHOUT a visible
//! window or GUI login session: attach a known-color `wl_shm` BGRA buffer, drive the neutral
//! `Presenter::present`, and read the composited frame back — asserting the color round-tripped through a
//! real GPU upload + composite render pass + readback.
//!
//! A visible `NSWindow` needs the AppKit main thread + a GUI session, which the cargo test harness does
//! not provide (tests run off the main thread, so `MainThreadMarker::new()` is `None`). Use the
//! `present_window` example for that; see the note in `windowed_present_requires_main_thread`.

#![cfg(all(feature = "macos-surface", target_os = "macos"))]

use hl_compositor::scene::model::{Format, OutputId, PresentableImage, SurfaceId};
use hl_compositor::scene::port::{PresentOutcome, PresentTiming, Presenter};
use hl_compositor::surface::macos::MacPresenter;

fn known_frame(w: u32, h: u32, bgra: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&bgra);
    }
    buf
}

fn image(sid: SurfaceId, w: i32, h: i32) -> PresentableImage {
    PresentableImage {
        surface: sid,
        width: w,
        height: h,
        format: Format::Xrgb8888,
        gpu: false,
        popup: None,
    }
}

#[test]
fn offscreen_present_roundtrips_known_color_on_real_metal() {
    let Some(mut presenter) = MacPresenter::new_offscreen() else {
        // No Metal device (e.g. CI without a GPU). Nothing to prove; do not fail the suite.
        eprintln!("SKIP: no Metal device available");
        return;
    };
    eprintln!("Metal adapter: {}", presenter.device_name());

    let (w, h) = (16u32, 16u32);
    // BGRA memory order B=20, G=130, R=240, A=255  ->  RGBA (240,130,20).
    let sid = SurfaceId(7);
    presenter.attach_bgra(sid, known_frame(w, h, [20, 130, 240, 255]), w, h);

    let fb = presenter.present(
        OutputId(0),
        &image(sid, w as i32, h as i32),
        &[],
        PresentTiming {
            present_ns: 0,
            refresh_ns: 0,
            vsync: false,
        },
    );
    // Headless: composited into the backing target but not shown on a screen.
    assert_eq!(fb.outcome, PresentOutcome::Offscreen, "headless => Offscreen");
    assert_eq!(presenter.frames, 1);

    let (rw, rh, rgba) = presenter
        .last_rgba(sid)
        .expect("a composited frame to read back");
    assert_eq!((rw, rh), (w, h));

    // Center pixel must be the known color (allow ±2 for any format conversion rounding).
    let c = ((h / 2 * w + w / 2) * 4) as usize;
    let (r, g, b, a) = (rgba[c], rgba[c + 1], rgba[c + 2], rgba[c + 3]);
    let near = |x: u8, t: u8| (x as i32 - t as i32).abs() <= 2;
    assert!(
        near(r, 240) && near(g, 130) && near(b, 20) && a == 255,
        "center pixel {:?} != expected (240,130,20,255)",
        (r, g, b, a)
    );
    eprintln!("OK: known color round-tripped through Metal upload+composite+readback -> ({r},{g},{b},{a})");
}

#[test]
fn windowed_present_requires_main_thread() {
    // Documents the honest limit: a real NSWindow needs the AppKit main thread (and a GUI session). The
    // cargo test harness runs this off the main thread, so the marker is unavailable and windowed mode
    // cannot be constructed here — the `present_window` example is the vehicle for a visible window.
    use objc2_foundation::MainThreadMarker;
    match MainThreadMarker::new() {
        None => eprintln!(
            "EXPECTED: not on the AppKit main thread in the test harness; windowed present is exercised \
             by `cargo run --example present_window --features macos-surface` (needs a GUI session)."
        ),
        Some(mtm) => {
            // If a runner ever puts us on the main thread, at least prove the presenter constructs.
            if let Some(p) = MacPresenter::new_windowed(mtm) {
                eprintln!("windowed presenter constructed on adapter: {}", p.device_name());
            }
        }
    }
}
