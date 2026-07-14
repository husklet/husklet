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
    use hl_compositor::scene::model::{Format, OutputId, PresentableImage, SurfaceId};
    use hl_compositor::scene::port::{PresentOutcome, PresentTiming, Presenter};
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
    presenter.set_title(sid, "hl present_window smoke");
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
    };

    let mut delivered = false;
    let run_loop = unsafe { NSRunLoop::currentRunLoop() };
    for _ in 0..60 {
        presenter.attach_bgra(sid, frame.clone(), w, h);
        let fb = presenter.present(
            OutputId(0),
            &img,
            &[],
            PresentTiming {
                present_ns: 0,
                refresh_ns: 0,
                vsync: false,
            },
        );
        if matches!(fb.outcome, PresentOutcome::Delivered { .. }) {
            delivered = true;
        }
        let until = unsafe { NSDate::dateWithTimeIntervalSinceNow(0.05) };
        unsafe { run_loop.runUntilDate(&until) };
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
