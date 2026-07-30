//! `cargo run --example present_window --features macos-surface` — open a REAL macOS window and present
//! a known-color frame through the Cocoa/Metal presenter. An example (not a test) so `main` runs on the
//! AppKit main thread, which a visible `NSWindow` requires; it additionally needs a GUI login session
//! (run it from a graphical session, not a bare SSH shell).
//!
//! It opens the window, presents a solid color, pumps the run loop for ~3s so the window becomes visible
//! and a `CAMetalLayer` drawable is vended, then exits — reporting whether the frame was Delivered
//! (window on screen) or stayed Offscreen (no GUI session).

#[cfg(all(feature = "macos-surface", target_os = "macos"))]
fn main() {
    use hl_compositor::scene::model::{
        BufferTransform, Format, OutputId, PresentableImage, Rect, SurfaceId, Visibility,
        WindowKind, WindowState,
    };
    use hl_compositor::scene::port::{
        HostEvents, PresentFrame, PresentLayer, PresentOutcome, PresentTiming, Presenter, Windows,
    };

    /// Build the one-role, one-layer frame the neutral presenter takes.
    fn single_layer(
        output: OutputId,
        image: PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentFrame {
        PresentFrame {
            output,
            role: image.surface,
            origin: (0, 0),
            layers: vec![PresentLayer {
                image,
                x: 0,
                y: 0,
                damage: damage.to_vec(),
            }],
            timing,
        }
    }
    use hl_compositor::surface::macos::MacPresenter;
    use objc2_foundation::{MainThreadMarker, NSDate, NSRunLoop};

    let mtm = MainThreadMarker::new().expect("examples run on the main thread");
    let Some(mut presenter) = MacPresenter::new_windowed(mtm) else {
        eprintln!("no Metal device available");
        return;
    };
    eprintln!("Metal adapter: {}", presenter.device_name());

    let (w, h) = (320u32, 200u32);
    let sid = SurfaceId(1);
    presenter.reconcile_window(&WindowState {
        surface: sid,
        kind: WindowKind::Toplevel { parent: None },
        title: "hl present_window smoke".into(),
        logical_size: Some((w as i32, h as i32)),
        min_size: (None, None),
        max_size: (None, None),
        maximized: false,
        fullscreen: false,
        geometry: None,
        visibility: Visibility::Visible,
    });
    // Solid teal: BGRA (B=200,G=120,R=20,A=255) -> RGBA (20,120,200).
    let frame = {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for px in v.chunks_exact_mut(4) {
            px.copy_from_slice(&[200, 120, 20, 255]);
        }
        v
    };

    let img = PresentableImage {
        surface: sid,
        width: w as i32,
        height: h as i32,
        format: Format::Xrgb8888,
        gpu: false,
        popup: None,
        present_crop: None,
        transform: BufferTransform::Normal,
    };

    let mut delivered = false;
    let run_loop = unsafe { NSRunLoop::currentRunLoop() };
    // Whole-surface damage every frame. The composite target is scissored to the frame's damage, so a
    // run of empty-damage presents renders the FIRST frame and then repeats it: the window would look
    // right while 59 of the 60 presents drew nothing.
    let whole = [Rect::new(0, 0, w as i32, h as i32)];
    let mut last_blue = 0u8;
    for tick in 0..60u32 {
        presenter.poll_events();
        // A slow blue ramp, so a watching human sees a LIVE window rather than a still image, and the
        // readback below can name which frame it is looking at.
        last_blue = (140 + tick * 2) as u8;
        let mut frame = frame.clone();
        for px in frame.chunks_exact_mut(4) {
            px[0] = last_blue;
        }
        presenter.attach_bgra(sid, frame, w, h);
        let fb = presenter.present_frame(&single_layer(
            OutputId(0),
            img.clone(),
            &whole,
            PresentTiming {
                present_ns: 0,
                refresh_ns: 0,
                vsync: false,
            },
        ));
        if matches!(fb.outcome, PresentOutcome::Delivered { .. }) {
            delivered = true;
        }
        let until = unsafe { NSDate::dateWithTimeIntervalSinceNow(0.05) };
        unsafe { run_loop.runUntilDate(&until) };
        presenter.poll_events();
    }

    let (capture_w, capture_h, rgba) = presenter
        .last_rgba(sid)
        .expect("the visible frame remains capturable");
    assert_eq!((capture_w, capture_h), (w, h));
    let center = ((capture_h / 2 * capture_w + capture_w / 2) * 4) as usize;
    // The LAST frame of the ramp, not the first: proof every present actually composited.
    assert_eq!(&rgba[center..center + 4], &[20, 120, last_blue, 255]);
    if let Ok(path) = std::env::var("HL_CAPTURE_PATH") {
        let mut ppm = format!("P6\n{capture_w} {capture_h}\n255\n").into_bytes();
        ppm.extend(
            rgba.chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]),
        );
        std::fs::write(&path, ppm).expect("write captured visible-window frame");
        println!("present_window: captured composited content at {path}");
    }

    if delivered {
        println!("present_window: frame Delivered to a visible NSWindow on a real Metal GPU");
    } else {
        println!(
            "present_window: frame composited but stayed Offscreen — no drawable vended (likely no GUI \
             login session). The offscreen readback smoke still proves the Metal pixel path."
        );
    }
}

#[cfg(not(all(feature = "macos-surface", target_os = "macos")))]
fn main() {
    eprintln!("present_window requires --features macos-surface on macOS");
}
