//! `dd-compositor` binary: the Smithay-native compositor endpoint, gated behind `DD_DISPLAY_SMITHAY=1`
//! (see `dd-display`'s launcher, which execs this binary when the flag is set). It binds the Wayland
//! socket, accepts guest clients, and composites their surfaces through the reused `Presenter` seam:
//!   - default on macOS: the native Cocoa/Metal window backend (one NSWindow per surface, HiDPI);
//!   - `--png <dir>` (any platform): dump each committed surface to a PNG — the headless proof path.
//!
//! Usage:  dd-compositor [--socket <path>] [--png <dir>] [--metal]
//! Env:    WAYLAND_DISPLAY / XDG_RUNTIME_DIR pick the socket when `--socket` is absent.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use dd_compositor::{ClientState, DdState};
use dd_display::present::{PngPresenter, Presenter};

use smithay::reexports::{
    calloop::{generic::Generic, EventLoop, Interest, Mode as CalloopMode, PostAction},
    wayland_server::Display,
};

struct LoopData {
    state: DdState,
    display: Display<DdState>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut socket: Option<String> = None;
    let mut png_dir: Option<String> = None;
    let mut metal = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next(),
            "--png" => png_dir = args.next(),
            "--metal" => metal = true,
            "--no-metal" => metal = false,
            other => eprintln!("dd-compositor: ignoring unknown arg {other:?}"),
        }
    }

    let socket = socket.unwrap_or_else(default_socket_path);

    // On macOS with no --png, run the native present/input loop. Otherwise the portable headless loop.
    #[cfg(target_os = "macos")]
    {
        if png_dir.is_none() {
            macos::run(&socket, metal);
        }
    }
    let _ = metal;

    let dir = png_dir.unwrap_or_else(|| "/tmp/dd-compositor-live".into());
    run_headless(&socket, Box::new(PngPresenter::new(dir)));
}

fn default_socket_path() -> String {
    let rt = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let name = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    if name.starts_with('/') {
        name
    } else {
        format!("{rt}/{name}")
    }
}

/// Bind the socket, build the calloop loop (listen source + wl_display source), and dispatch forever.
/// Shared by the headless path and (via the injected per-iteration callback) the macOS path.
fn build_loop(
    socket: &str,
    presenter: Box<dyn Presenter>,
) -> (EventLoop<'static, LoopData>, LoopData) {
    let event_loop: EventLoop<LoopData> = EventLoop::try_new().expect("calloop event loop");
    let mut display: Display<DdState> = Display::new().expect("wl_display");
    let dh = display.handle();

    let state = DdState::new(dh, presenter);

    // Listening socket. dd's daemon injects a known socket path, so bind it directly (parity with
    // dd-display's `listen_unix`) rather than auto-picking a wayland-N name.
    let lfd = dd_display::listen_unix(socket).expect("bind wayland socket");
    set_nonblock(lfd);
    eprintln!("dd-compositor: listening on {socket}");
    let listener = unsafe { OwnedFd::from_raw_fd(lfd) };

    let handle = event_loop.handle();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, CalloopMode::Level),
            |_, listener, data: &mut LoopData| {
                let lfd = listener.as_raw_fd();
                loop {
                    let cfd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
                    if cfd < 0 {
                        break; // EWOULDBLOCK: drained
                    }
                    set_nonblock(cfd);
                    let stream = unsafe { UnixStream::from_raw_fd(cfd) };
                    match data
                        .display
                        .handle()
                        .insert_client(stream, Arc::new(ClientState::default()))
                    {
                        Ok(_) => eprintln!("dd-compositor: client connected"),
                        Err(e) => eprintln!("dd-compositor: insert_client failed: {e}"),
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("insert socket source");

    // Drive the wl_display fd so client requests dispatch.
    let display_fd = display
        .backend()
        .poll_fd()
        .try_clone_to_owned()
        .expect("dup display fd");
    handle
        .insert_source(
            Generic::new(display_fd, Interest::READ, CalloopMode::Level),
            |_, _, data: &mut LoopData| {
                data.display
                    .dispatch_clients(&mut data.state)
                    .expect("dispatch clients");
                Ok(PostAction::Continue)
            },
        )
        .expect("insert display source");

    (event_loop, LoopData { state, display })
}

fn run_headless(socket: &str, presenter: Box<dyn Presenter>) -> ! {
    let (mut event_loop, mut data) = build_loop(socket, presenter);
    eprintln!("dd-compositor: entering calloop (headless)");
    loop {
        event_loop
            .dispatch(Some(std::time::Duration::from_millis(16)), &mut data)
            .expect("dispatch");
        let _ = data.display.flush_clients();
    }
}

fn set_nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

// ===================================== macOS native present/input =====================================

#[cfg(target_os = "macos")]
mod macos {
    use super::{build_loop, LoopData};
    use dd_display::present::Presenter;
    use dd_display::present_cocoa::{CocoaPresenter, MetalPresenter};
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags,
        NSEventType,
    };
    use objc2_foundation::{MainThreadMarker, NSDefaultRunLoopMode, NSPoint};

    /// The native loop: create the NSApplication + Cocoa/Metal presenter on the main thread, then run
    /// calloop with a short timeout, draining AppKit events into the Smithay seat each iteration. This
    /// mirrors `dd-display`'s `present_cocoa::drive`, but the compositor core underneath is Smithay.
    pub fn run(socket: &str, metal: bool) -> ! {
        let mtm = MainThreadMarker::new().expect("dd-compositor must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        unsafe { app.finishLaunching() };
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);

        let presenter: Box<dyn Presenter> = if metal {
            dd_display::metal::start_gpu_bridge();
            match MetalPresenter::new(mtm) {
                Some(mp) => Box::new(mp),
                None => {
                    eprintln!("dd-compositor: no Metal device; using NSImageView presenter");
                    Box::new(CocoaPresenter::new(mtm))
                }
            }
        } else {
            Box::new(CocoaPresenter::new(mtm))
        };

        let (mut event_loop, mut data) = build_loop(socket, presenter);
        eprintln!("dd-compositor[cocoa]: entering calloop (metal={metal})");
        loop {
            // CRITICAL: service AppKit input BEFORE the calloop dispatch (which runs commit→present).
            // Input must never be gated behind a present — this is the same latency lesson being fixed
            // on the legacy path (dd-display: "service NSEvents before pump"). Present is non-blocking
            // (the Metal path never blocks on nextDrawable), so a slow frame cannot stall the pointer.
            drain_appkit(&app, &mut data);
            let _ = data.display.flush_clients();
            // Short timeout so we loop back to drain input promptly even when no client fd is readable.
            event_loop
                .dispatch(Some(std::time::Duration::from_millis(8)), &mut data)
                .expect("dispatch");
            drain_appkit(&app, &mut data);
            let _ = data.display.flush_clients();
        }
    }

    /// Drain queued AppKit events: route input into the Smithay seat, then forward to AppKit so window
    /// chrome stays responsive.
    fn drain_appkit(app: &Retained<NSApplication>, data: &mut LoopData) {
        // Resolve the focused surface's on-screen size + input scale so Cocoa's bottom-left point coords
        // map into the client's top-left surface space. `surface_size` is the window size in POINTS and
        // `surface_scale` is device-pixels-per-point for the input path (1.0 for the point-space present,
        // matching the legacy dd-display live loop); threading both keeps clicks landing correctly on a
        // 2x Retina backing store instead of assuming a fixed 1.0 scale.
        let sid = data.state.focus.as_ref().map(|s| {
            use smithay::reexports::wayland_server::Resource;
            s.id().protocol_id()
        });
        let flip_h = sid
            .and_then(|sid| data.state.presenter.surface_size(sid))
            .map(|(_, h)| h);
        let scale = sid
            .map(|sid| data.state.presenter.surface_scale(sid))
            .unwrap_or(1.0);

        loop {
            let ev = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    None,
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            match ev {
                Some(ev) => {
                    inject(&mut data.state, &ev, flip_h, scale);
                    unsafe { app.sendEvent(&ev) };
                }
                None => break,
            }
        }
    }

    /// Flip Cocoa's bottom-left `locationInWindow` (points) into top-left surface space, scaling points
    /// into the surface's input coordinate space. `flip_h` and the scaled Y share the same (point) domain
    /// when `scale == 1.0`; a >1 scale keeps X/Y consistent for a device-pixel input path. Mirrors
    /// dd-display's `present_cocoa::flip_point`.
    fn flip_point(p: NSPoint, flip_h: Option<i32>, scale: f64) -> (f64, f64) {
        let scale = scale.max(1.0);
        let x = (p.x * scale).max(0.0);
        let py = p.y * scale;
        let y = match flip_h {
            Some(h) if h > 0 => {
                let hp = (h as f64) * scale;
                (hp - py).clamp(0.0, hp - 1.0)
            }
            _ => py.max(0.0),
        };
        (x, y)
    }

    /// Translate an `NSEvent` into `wl_seat` input on the Smithay compositor. Keyboard uses the
    /// `kVK_*`→evdev subset below; the guest's own xkbcommon (fed Smithay's keymap) resolves the sym.
    fn inject(state: &mut crate::DdState, ev: &NSEvent, flip_h: Option<i32>, scale: f64) {
        let ty = unsafe { ev.r#type() };
        match ty {
            NSEventType::MouseMoved
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDragged => {
                let (x, y) = flip_point(unsafe { ev.locationInWindow() }, flip_h, scale);
                state.pointer_motion(x, y);
            }
            NSEventType::LeftMouseDown => {
                let (x, y) = flip_point(unsafe { ev.locationInWindow() }, flip_h, scale);
                state.pointer_motion(x, y);
                state.pointer_button(0x110, true);
            }
            NSEventType::LeftMouseUp => {
                let (x, y) = flip_point(unsafe { ev.locationInWindow() }, flip_h, scale);
                state.pointer_motion(x, y);
                state.pointer_button(0x110, false);
            }
            NSEventType::RightMouseDown => {
                let (x, y) = flip_point(unsafe { ev.locationInWindow() }, flip_h, scale);
                state.pointer_motion(x, y);
                state.pointer_button(0x111, true);
            }
            NSEventType::RightMouseUp => {
                let (x, y) = flip_point(unsafe { ev.locationInWindow() }, flip_h, scale);
                state.pointer_motion(x, y);
                state.pointer_button(0x111, false);
            }
            NSEventType::ScrollWheel => {
                let dy = unsafe { ev.scrollingDeltaY() };
                let dx = unsafe { ev.scrollingDeltaX() };
                let precise = unsafe { ev.hasPreciseScrollingDeltas() };
                // Wayland axis is positive = down/right; Cocoa natural scroll is inverted.
                let vy = -dy;
                let vx = -dx;
                if vx != 0.0 || vy != 0.0 {
                    state.pointer_axis(vx, vy, precise);
                }
            }
            NSEventType::KeyDown => {
                if let Some(code) = kvk_to_evdev(unsafe { ev.keyCode() }) {
                    state.key(code, true);
                }
            }
            NSEventType::KeyUp => {
                if let Some(code) = kvk_to_evdev(unsafe { ev.keyCode() }) {
                    state.key(code, false);
                }
            }
            _ => {}
        }
        let _ = NSEventModifierFlags::NSEventModifierFlagShift; // reserved for the modifiers follow-up.
    }

    /// macOS virtual keycode (`kVK_*`) → Linux evdev `KEY_*` (alphanumerics + common keys). A subset
    /// for the first increment; the full table is a follow-up (dd-display carries the larger map).
    fn kvk_to_evdev(kvk: u16) -> Option<u32> {
        Some(match kvk {
            0 => 30, 1 => 31, 2 => 32, 3 => 33, 4 => 35, 5 => 34, 6 => 44, 7 => 45, 8 => 46,
            9 => 47, 11 => 48, 12 => 16, 13 => 17, 14 => 18, 15 => 19, 16 => 21, 17 => 20,
            31 => 24, 32 => 22, 34 => 23, 35 => 25, 37 => 38, 38 => 36, 40 => 37, 45 => 49,
            46 => 50, 18 => 2, 19 => 3, 20 => 4, 21 => 5, 22 => 7, 23 => 6, 25 => 10, 26 => 8,
            28 => 9, 29 => 11, 36 => 28, 48 => 15, 49 => 57, 51 => 14, 53 => 1,
            123 => 105, 124 => 106, 125 => 108, 126 => 103,
            _ => return None,
        })
    }
}
