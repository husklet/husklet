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

use hl_compositor::scene::model::{
    BufferTransform, Format, OutputId, PresentableImage, Rect, SurfaceId, Visibility, WindowKind,
    WindowState,
};
use hl_compositor::scene::port::{PresentOutcome, PresentTiming, Presenter, Windows};
use hl_compositor::surface::macos::MacPresenter;

fn known_frame(w: u32, h: u32, bgra: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&bgra);
    }
    buf
}

fn image_with_format(sid: SurfaceId, w: i32, h: i32, format: Format) -> PresentableImage {
    PresentableImage {
        surface: sid,
        width: w,
        height: h,
        format,
        gpu: false,
        popup: None,
        present_crop: None,
        transform: BufferTransform::Normal,
    }
}

fn image(sid: SurfaceId, w: i32, h: i32) -> PresentableImage {
    image_with_format(sid, w, h, Format::Xrgb8888)
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
    assert_eq!(
        fb.outcome,
        PresentOutcome::Offscreen,
        "headless => Offscreen"
    );
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
fn argb_present_preserves_transparent_window_pixels_on_real_metal() {
    let Some(mut presenter) = MacPresenter::new_offscreen() else {
        eprintln!("SKIP: no Metal device available");
        return;
    };

    let sid = SurfaceId(8);
    presenter.attach_bgra(sid, known_frame(4, 4, [20, 130, 240, 37]), 4, 4);
    presenter.present(
        OutputId(0),
        &image_with_format(sid, 4, 4, Format::Argb8888),
        &[],
        PresentTiming {
            present_ns: 0,
            refresh_ns: 0,
            vsync: false,
        },
    );

    let (_, _, rgba) = presenter.last_rgba(sid).expect("ARGB frame readback");
    assert_eq!(&rgba[..4], &[240, 130, 20, 37]);
}

#[test]
fn xrgb_present_forces_opaque_window_pixels_on_real_metal() {
    let Some(mut presenter) = MacPresenter::new_offscreen() else {
        eprintln!("SKIP: no Metal device available");
        return;
    };

    let sid = SurfaceId(9);
    presenter.attach_bgra(sid, known_frame(4, 4, [20, 130, 240, 0]), 4, 4);
    presenter.present(
        OutputId(0),
        &image(sid, 4, 4),
        &[],
        PresentTiming {
            present_ns: 0,
            refresh_ns: 0,
            vsync: false,
        },
    );

    let (_, _, rgba) = presenter.last_rgba(sid).expect("XRGB frame readback");
    assert_eq!(&rgba[..4], &[240, 130, 20, 255]);
}

#[test]
fn malformed_bgra_is_rejected_before_metal_reads_it() {
    let Some(mut presenter) = MacPresenter::new_offscreen() else {
        eprintln!("SKIP: no Metal device available");
        return;
    };

    let sid = SurfaceId(10);
    presenter.attach_bgra(sid, vec![0; 15], 2, 2);
    let feedback = presenter.present(
        OutputId(0),
        &image(sid, 2, 2),
        &[],
        PresentTiming {
            present_ns: 0,
            refresh_ns: 0,
            vsync: false,
        },
    );
    assert_eq!(feedback.outcome, PresentOutcome::RetryableFailure);
    assert_eq!(presenter.frames, 0);
    assert!(presenter.last_rgba(sid).is_none());
}

#[test]
fn requested_capture_writes_only_the_next_presented_frame() {
    let Some(presenter) = MacPresenter::new_offscreen() else {
        return;
    };
    let directory =
        std::env::temp_dir().join(format!("hl-one-shot-present-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut presenter = presenter.capture_once_to(&directory).unwrap();
    let sid = SurfaceId(14);
    let timing = PresentTiming {
        present_ns: 0,
        refresh_ns: 0,
        vsync: false,
    };

    presenter.attach_bgra(sid, known_frame(2, 2, [1, 2, 3, 255]), 2, 2);
    presenter.present(OutputId(0), &image(sid, 2, 2), &[], timing);
    let output = directory.join("surface-14.ppm");
    assert!(!output.exists(), "capture stays idle without a request");

    std::fs::write(directory.join("request"), []).unwrap();
    presenter.attach_bgra(sid, known_frame(2, 2, [4, 5, 6, 255]), 2, 2);
    presenter.present(OutputId(0), &image(sid, 2, 2), &[], timing);
    let captured = std::fs::read(&output).unwrap();

    presenter.attach_bgra(sid, known_frame(2, 2, [7, 8, 9, 255]), 2, 2);
    presenter.present(OutputId(0), &image(sid, 2, 2), &[], timing);
    assert_eq!(
        std::fs::read(&output).unwrap(),
        captured,
        "one request must not continuously overwrite captures"
    );
    assert!(directory.join("request.claimed").is_file());
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn requested_capture_reads_the_latest_static_frame_during_event_poll() {
    use hl_compositor::scene::port::HostEvents;

    let Some(presenter) = MacPresenter::new_offscreen() else {
        return;
    };
    let directory = std::env::temp_dir().join(format!("hl-static-present-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let mut presenter = presenter.capture_once_to(&directory).unwrap();
    let sid = SurfaceId(15);
    let timing = PresentTiming {
        present_ns: 0,
        refresh_ns: 0,
        vsync: false,
    };

    presenter.attach_bgra(sid, known_frame(2, 2, [11, 22, 33, 255]), 2, 2);
    presenter.present(OutputId(0), &image(sid, 2, 2), &[], timing);
    std::fs::write(directory.join("request"), []).unwrap();

    presenter.poll_events();

    assert_eq!(
        std::fs::read(directory.join("surface-15.ppm")).unwrap(),
        b"P6\n2 2\n255\n\x21\x16\x0b\x21\x16\x0b\x21\x16\x0b\x21\x16\x0b"
    );
    assert!(directory.join("request.claimed").is_file());
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn rotating_upload_refreshes_the_selected_slot_completely() {
    let Some(mut presenter) = MacPresenter::new_offscreen() else {
        return;
    };
    let sid = SurfaceId(11);
    let timing = PresentTiming {
        present_ns: 0,
        refresh_ns: 0,
        vsync: false,
    };
    for color in [[10, 20, 30, 255], [40, 50, 60, 255], [70, 80, 90, 255]] {
        presenter.attach_bgra(sid, known_frame(4, 4, color), 4, 4);
        presenter.present(OutputId(0), &image(sid, 4, 4), &[], timing);
    }

    let mut fourth = known_frame(4, 4, [70, 80, 90, 255]);
    fourth[..4].copy_from_slice(&[1, 2, 3, 255]);
    presenter.attach_bgra_damage(sid, fourth, 4, 4, Some(vec![Rect::new(0, 0, 1, 1)]));
    presenter.present(OutputId(0), &image(sid, 4, 4), &[], timing);

    let (_, _, rgba) = presenter.last_rgba(sid).expect("damaged frame readback");
    assert_eq!(&rgba[..4], &[3, 2, 1, 255], "damaged pixel updated");
    assert_eq!(
        &rgba[4..8],
        &[90, 80, 70, 255],
        "the selected slot receives the complete current frame, not only damage relative to another slot"
    );
}

#[test]
fn xdg_window_geometry_crops_client_shadow_margins() {
    let Some(mut presenter) = MacPresenter::new_offscreen() else {
        return;
    };
    let sid = SurfaceId(12);
    presenter.reconcile_window(&WindowState {
        surface: sid,
        kind: WindowKind::Toplevel { parent: None },
        title: "geometry".into(),
        logical_size: Some((4, 4)),
        min_size: (None, None),
        max_size: (None, None),
        maximized: false,
        fullscreen: false,
        geometry: Some(Rect::new(1, 1, 2, 2)),
        visibility: Visibility::Visible,
    });
    let mut frame = known_frame(4, 4, [0, 0, 0, 0]);
    for y in 1..3 {
        for x in 1..3 {
            let offset = ((y * 4 + x) * 4) as usize;
            frame[offset..offset + 4].copy_from_slice(&[20, 130, 240, 255]);
        }
    }
    presenter.attach_bgra(sid, frame, 4, 4);
    let mut image = image(sid, 4, 4);
    image.present_crop = Some((0.0, 0.0, 4.0, 4.0));
    presenter.present(
        OutputId(0),
        &image,
        &[],
        PresentTiming {
            present_ns: 0,
            refresh_ns: 0,
            vsync: false,
        },
    );
    let (w, h, rgba) = presenter.last_rgba(sid).expect("cropped frame");
    assert_eq!((w, h), (2, 2));
    assert_eq!(&rgba[..4], &[240, 130, 20, 255]);
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
