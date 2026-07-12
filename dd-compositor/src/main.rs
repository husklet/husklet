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
                // ROBUSTNESS: a client protocol error is handled by wayland-server internally (that one
                // client is disconnected); dispatch_clients only returns Err on a catastrophic backend
                // fault. Either way, NEVER panic here — the whole compositor (and every other guest window)
                // must survive one misbehaving client. Log and keep dispatching.
                if let Err(e) = data.display.dispatch_clients(&mut data.state) {
                    eprintln!("dd-compositor: dispatch_clients error (continuing): {e}");
                }
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
        // ROBUSTNESS: a calloop dispatch error must not abort the compositor. Client-triggered faults are
        // absorbed inside the source callbacks (which never panic); log any loop-level error and continue.
        if let Err(e) = event_loop.dispatch(Some(std::time::Duration::from_millis(16)), &mut data) {
            eprintln!("dd-compositor: event loop dispatch error (continuing): {e}");
        }
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
    use std::os::fd::{FromRawFd, OwnedFd};

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
            // A host-driven NSWindow resize (user dragged the window edge) reflows the focused client:
            // observe the live AppKit content size and, on a change, send xdg_toplevel.configure so the
            // guest repaints at the new size. Debounced inside maybe_resize_focused (mirrors the legacy
            // dd-display live loop's Server::maybe_resize). Cheap when nothing changed.
            data.state.maybe_resize_focused();
            let _ = data.display.flush_clients();
            // Short timeout so we loop back to drain input promptly even when no client fd is readable.
            // ROBUSTNESS: never panic on a dispatch error — one bad client (or a transient loop fault)
            // must not take down the compositor and every other guest window. Log and keep running.
            if let Err(e) = event_loop.dispatch(Some(std::time::Duration::from_millis(8)), &mut data) {
                eprintln!("dd-compositor: event loop dispatch error (continuing): {e}");
            }
            drain_appkit(&app, &mut data);
            data.state.maybe_resize_focused();
            // Bridge the clipboard both ways once per iteration: mirror any host-clipboard change into a
            // guest-facing selection (paste), and export any guest copy to the host clipboard.
            data.state.offer_host_clipboard();
            pump_guest_copy(&mut data);
            let _ = data.display.flush_clients();
        }
    }

    /// Guest → host clipboard export. When a guest sets its selection (a copy), `SelectionHandler::
    /// new_selection` queues the offered mime types on `DdState::pending_host_copy`; here we read the guest
    /// source's bytes through a pipe and push them onto the host clipboard via `Presenter::clipboard_set_host`.
    ///
    /// The guest source writes asynchronously (it lives in another process and only writes once dispatched),
    /// so we drive a few bounded dispatch+read passes on a non-blocking pipe rather than blocking the loop.
    /// Chrome/GTK answer promptly; if a pass reads nothing the next copy simply retries.
    fn pump_guest_copy(data: &mut LoopData) {
        use smithay::wayland::selection::data_device::request_data_device_client_selection;

        let Some(mimes) = data.state.take_pending_host_copy() else {
            return;
        };
        if mimes.is_empty() {
            return;
        }
        // Prefer a UTF-8 text flavour (what a host paste target most wants); fall back to the first offered.
        let mime = mimes
            .iter()
            .find(|m| m.starts_with("text/plain") || m.contains("utf-8") || m.contains("UTF8"))
            .or_else(|| mimes.first())
            .cloned()
            .unwrap();

        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        let (rd, wr) = (fds[0], fds[1]);
        super::set_nonblock(rd);
        let owned_wr = unsafe { OwnedFd::from_raw_fd(wr) };
        // Hands `wr` to the guest source as wl_data_source.send(mime, wr); our copy is dropped here.
        if request_data_device_client_selection(&data.state.seat, mime.clone(), owned_wr).is_err() {
            unsafe { libc::close(rd) };
            return;
        }

        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        for _ in 0..8 {
            let _ = data.display.dispatch_clients(&mut data.state);
            let _ = data.display.flush_clients();
            let n = unsafe { libc::read(rd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                bytes.extend_from_slice(&buf[..n as usize]);
            } else if n == 0 {
                break; // writer closed → EOF, transfer complete.
            }
        }
        unsafe { libc::close(rd) };

        if !bytes.is_empty() {
            data.state.presenter.clipboard_set_host(&mime, &bytes);
            // Adopt the resulting host generation as already-mirrored so `offer_host_clipboard` does not
            // bounce our own push back to the guest as a new selection.
            data.state.mark_host_clipboard_synced();
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
                // Keep the modifier state fresh even when a chord's flag change coalesced into the key
                // event without a standalone FlagsChanged (AppKit can do this), then deliver the key.
                state.update_modifiers(mods_mask(ev));
                if let Some(code) = kvk_to_evdev(unsafe { ev.keyCode() }) {
                    state.key(code, true);
                }
            }
            NSEventType::KeyUp => {
                state.update_modifiers(mods_mask(ev));
                if let Some(code) = kvk_to_evdev(unsafe { ev.keyCode() }) {
                    state.key(code, false);
                }
            }
            NSEventType::FlagsChanged => {
                // Shift/Ctrl/Alt/Cmd/CapsLock changed: mirror the whole modifier level into the seat's XKB
                // state so shortcuts and cased typing work. `update_modifiers` diffs and emits
                // wl_keyboard.modifiers (see handlers/seat.rs).
                state.update_modifiers(mods_mask(ev));
            }
            _ => {}
        }
    }

    /// Collapse an `NSEvent`'s modifier flags into the device-independent bitmask
    /// `DdState::update_modifiers` expects (bit0 Shift, bit1 Ctrl, bit2 Alt, bit3 Super/Cmd, bit4 CapsLock).
    fn mods_mask(ev: &NSEvent) -> u32 {
        let f = unsafe { ev.modifierFlags() };
        let mut mask = 0u32;
        if f.contains(NSEventModifierFlags::NSEventModifierFlagShift) {
            mask |= 0b0_0001;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagControl) {
            mask |= 0b0_0010;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagOption) {
            mask |= 0b0_0100;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagCommand) {
            mask |= 0b0_1000;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagCapsLock) {
            mask |= 0b1_0000;
        }
        mask
    }

    /// macOS virtual keycode (`kVK_*`, Carbon HIToolbox) → Linux evdev `KEY_*`. Ported and completed from
    /// `dd-display`'s legacy map: alphanumerics, punctuation, the editing/navigation cluster, F1–F12, and
    /// the numeric keypad. The guest's own xkbcommon (fed the seat's keymap) turns the evdev code into a
    /// keysym, so this only has to be the physical-key correspondence. Modifier keys arrive as
    /// `FlagsChanged` (see `mods_mask`/`update_modifiers`), so they are intentionally absent here.
    fn kvk_to_evdev(kvk: u16) -> Option<u32> {
        Some(match kvk {
            // ---- letters (kVK_ANSI_A … Z) ----
            0 => 30, 1 => 31, 2 => 32, 3 => 33, 4 => 35, 5 => 34, 6 => 44, 7 => 45, 8 => 46,
            9 => 47, 11 => 48, 12 => 16, 13 => 17, 14 => 18, 15 => 19, 16 => 21, 17 => 20,
            31 => 24, 32 => 22, 34 => 23, 35 => 25, 37 => 38, 38 => 36, 40 => 37, 45 => 49,
            46 => 50,
            // ---- number row (kVK_ANSI_1 … 0) ----
            18 => 2, 19 => 3, 20 => 4, 21 => 5, 22 => 7, 23 => 6, 25 => 10, 26 => 8,
            28 => 9, 29 => 11,
            // ---- whitespace / editing ----
            36 => 28,  // Return       → KEY_ENTER
            48 => 15,  // Tab          → KEY_TAB
            49 => 57,  // Space        → KEY_SPACE
            51 => 14,  // Delete       → KEY_BACKSPACE
            53 => 1,   // Escape       → KEY_ESC
            // ---- punctuation (kVK_ANSI_* → KEY_*) ----
            27 => 12,  // Minus        → KEY_MINUS
            24 => 13,  // Equal        → KEY_EQUAL
            33 => 26,  // LeftBracket  → KEY_LEFTBRACE
            30 => 27,  // RightBracket → KEY_RIGHTBRACE
            41 => 39,  // Semicolon    → KEY_SEMICOLON
            39 => 40,  // Quote        → KEY_APOSTROPHE
            50 => 41,  // Grave        → KEY_GRAVE
            42 => 43,  // Backslash    → KEY_BACKSLASH
            43 => 51,  // Comma        → KEY_COMMA
            47 => 52,  // Period       → KEY_DOT
            44 => 53,  // Slash        → KEY_SLASH
            // ---- navigation / editing cluster ----
            123 => 105, // LeftArrow   → KEY_LEFT
            124 => 106, // RightArrow  → KEY_RIGHT
            125 => 108, // DownArrow   → KEY_DOWN
            126 => 103, // UpArrow     → KEY_UP
            115 => 102, // Home        → KEY_HOME
            119 => 107, // End         → KEY_END
            116 => 104, // PageUp      → KEY_PAGEUP
            121 => 109, // PageDown    → KEY_PAGEDOWN
            117 => 111, // ForwardDelete → KEY_DELETE
            114 => 110, // Help/Insert → KEY_INSERT
            // ---- function row F1–F12 ----
            122 => 59, 120 => 60, 99 => 61, 118 => 62, 96 => 63, 97 => 64, 98 => 65, 100 => 66,
            101 => 67, 109 => 68, 103 => 87, 111 => 88,
            // ---- numeric keypad (kVK_ANSI_Keypad*) ----
            82 => 82,  // Keypad0 → KEY_KP0
            83 => 79,  // Keypad1 → KEY_KP1
            84 => 80,  // Keypad2 → KEY_KP2
            85 => 81,  // Keypad3 → KEY_KP3
            86 => 75,  // Keypad4 → KEY_KP4
            87 => 76,  // Keypad5 → KEY_KP5
            88 => 77,  // Keypad6 → KEY_KP6
            89 => 71,  // Keypad7 → KEY_KP7
            91 => 72,  // Keypad8 → KEY_KP8
            92 => 73,  // Keypad9 → KEY_KP9
            65 => 83,  // KeypadDecimal  → KEY_KPDOT
            67 => 55,  // KeypadMultiply → KEY_KPASTERISK
            69 => 78,  // KeypadPlus     → KEY_KPPLUS
            71 => 69,  // KeypadClear    → KEY_NUMLOCK
            75 => 98,  // KeypadDivide   → KEY_KPSLASH
            76 => 96,  // KeypadEnter    → KEY_KPENTER
            78 => 74,  // KeypadMinus    → KEY_KPMINUS
            81 => 117, // KeypadEquals   → KEY_KPEQUAL
            _ => return None,
        })
    }
}
