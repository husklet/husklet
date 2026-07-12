//! Native popup-window placement parity (readiness Gap 2).
//!
//! With `DD_DISPLAY_POPUP_WINDOWS=1` the Smithay-native compositor presents an `xdg_popup` as its OWN
//! window (a menu/dropdown/tooltip) at the positioner-resolved anchor, instead of compositing it into —
//! and clipping it to — the owning toplevel's frame. This mirrors the legacy `server.rs`/`present_cocoa`
//! native-popup path (`SurfaceBuffer::popup` / `PopupPlacement`, commit 48f9bfe1).
//!
//! This drives a real in-process Wayland client through the toplevel + popup handshake and asserts the
//! popup's committed frame reaches the `Presenter` as its OWN `SurfaceBuffer` carrying a
//! `PopupPlacement { parent_sid, x, y }` that resolves to the anchor (the direct parent surface + the
//! positioner geometry origin), NOT composited into the toplevel. Runs headlessly (Linux + macOS).
//!
//! It is a SEPARATE test binary (not folded into `client_roundtrip`) because `wayland-server`'s `Display`
//! keeps process-global state and this test sets a process-global env var — isolation keeps both
//! deterministic.

use dd_compositor::{ClientState, DdState};
use dd_display::present::{Presenter, SurfaceBuffer};
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::{Arc, Mutex};

const WL_DISPLAY: u32 = 1;

/// The last popup `SurfaceBuffer` (its `sid` and `popup` placement) the compositor presented as its own
/// window. `None` while nothing with a popup placement has been presented.
type LastPopup = Arc<Mutex<Option<(u32, Option<dd_display::present::PopupPlacement>)>>>;

struct RecordingPresenter {
    frames: u32,
    /// Captures `(sid, popup)` of the last presented frame whose `popup` placement is `Some` — i.e. a
    /// popup presented as its own window rather than composited into a toplevel.
    last_popup: LastPopup,
}
impl Presenter for RecordingPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> bool {
        self.frames += 1;
        if surf.popup.is_some() {
            *self.last_popup.lock().unwrap() = Some((surf.sid, surf.popup));
        }
        true
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
}

/// A minimal Wayland client: enough wire to bind globals, map a toplevel, and map a popup off it.
struct Client {
    conn: Conn,
    next_id: u32,
    globals: std::collections::HashMap<String, (u32, u32)>,
    /// The object id whose `configure(serial)` (opcode 0) events we want to capture next.
    watch_xdg: u32,
    last_xdg_serial: Option<u32>,
}
impl Client {
    fn new(fd: RawFd) -> Client {
        Client {
            conn: Conn::new(fd),
            next_id: 2,
            globals: Default::default(),
            watch_xdg: 0,
            last_xdg_serial: None,
        }
    }
    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    fn flush(&mut self) {
        self.conn.flush().unwrap();
    }
    fn drain(&mut self) {
        loop {
            match self.conn.fill().unwrap() {
                0 | -1 => break,
                _ => {}
            }
        }
        while let Some(m) = self.conn.next_message() {
            if m.opcode == 0 && m.object == 2 {
                // wl_registry.global(name, iface, version)
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            } else if self.watch_xdg != 0 && m.object == self.watch_xdg && m.opcode == 0 {
                // xdg_surface.configure(serial)
                self.last_xdg_serial = Some(m.reader().u32());
            }
        }
    }
}

fn socketpair_nonblocking() -> (RawFd, RawFd) {
    let mut sv = [0i32; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
        0
    );
    for fd in sv {
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    (sv[0], sv[1])
}

#[test]
fn popup_presents_as_own_window_at_positioner_anchor() {
    // Enable the native popup-window path for this (isolated) test binary.
    std::env::set_var("DD_DISPLAY_POPUP_WINDOWS", "1");

    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let last_popup: LastPopup = Arc::new(Mutex::new(None));
    let mut state = DdState::new(
        dh.clone(),
        Box::new(RecordingPresenter { frames: 0, last_popup: last_popup.clone() }),
    );

    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();

    let mut c = Client::new(client_fd);
    macro_rules! pump {
        () => {{
            c.flush();
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }};
    }

    // Registry → globals.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();

    let bind = |c: &mut Client, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };
    let comp = bind(&mut c, "wl_compositor", 4);
    let shm = bind(&mut c, "wl_shm", 1);
    // xdg_wm_base at v3 for get_popup + positioner.
    let wm = bind(&mut c, "xdg_wm_base", 3);

    // Toplevel: surface + xdg_surface + toplevel + initial commit → configure → ack.
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    let xdg = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    let toplevel = c.alloc();
    c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.watch_xdg = xdg;
    c.conn.send(&Message::new(surface, 6)); // commit (no buffer) → configure
    pump!();
    let serial = c.last_xdg_serial.expect("toplevel xdg_surface.configure serial");
    c.conn.send(&Message::new(xdg, 4).u32(serial)); // ack_configure

    // Map the toplevel with a buffer so its window/sid exists (the popup window is a child of it).
    let (w, h): (i32, i32) = (4, 3);
    let stride = w * 4;
    let size = (stride * h) as usize;
    let pixels = vec![0x40u8; size];
    let mfd = dd_display::keymap::anon_fd_with(&pixels).expect("anon shm fd");
    let pool = c.alloc();
    c.conn.send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // create_pool (fd OOB)
    c.conn.queue_fd(mfd);
    pump!();
    unsafe { libc::close(mfd) };
    let buffer = c.alloc();
    c.conn.send(
        &Message::new(pool, 0)
            .u32(buffer)
            .i32(0)
            .i32(w)
            .i32(h)
            .i32(stride)
            .u32(1), // XRGB8888
    );
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(surface, 6)); // commit → present toplevel
    pump!();

    // Positioner: a 120x80 popup anchored at the bottom-left of a 200x24 rect at (10,30) in the parent's
    // window geometry, growing toward the bottom-right → resolves to origin (10, 54) (matching
    // client_roundtrip's positioner assertion).
    let pos = c.alloc();
    c.conn.send(&Message::new(wm, 1).u32(pos)); // create_positioner
    c.conn.send(&Message::new(pos, 1).i32(120).i32(80)); // set_size
    c.conn
        .send(&Message::new(pos, 2).i32(10).i32(30).i32(200).i32(24)); // set_anchor_rect
    c.conn.send(&Message::new(pos, 3).u32(6)); // set_anchor(bottom_left)
    c.conn.send(&Message::new(pos, 4).u32(8)); // set_gravity(bottom_right)

    // Popup: surface + xdg_surface + get_popup(parent = toplevel's xdg_surface) → configure → ack.
    let psurface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(psurface)); // create_surface
    let pxdg = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(pxdg).u32(psurface)); // get_xdg_surface
    let popup = c.alloc();
    c.conn.send(&Message::new(pxdg, 2).u32(popup).u32(xdg).u32(pos)); // get_popup(id, parent, positioner)
    c.watch_xdg = pxdg;
    c.last_xdg_serial = None;
    c.conn.send(&Message::new(psurface, 6)); // commit (no buffer) → popup configure
    pump!();
    let pserial = c.last_xdg_serial.expect("popup xdg_surface.configure serial");
    c.conn.send(&Message::new(pxdg, 4).u32(pserial)); // ack_configure

    // Commit a buffer to the popup → with DD_DISPLAY_POPUP_WINDOWS it presents as its OWN window.
    let (pw, ph): (i32, i32) = (8, 8);
    let ppixels = vec![0x90u8; (pw * ph * 4) as usize];
    let pmfd = dd_display::keymap::anon_fd_with(&ppixels).expect("popup anon shm fd");
    let ppool = c.alloc();
    c.conn
        .send(&Message::new(shm, 0).u32(ppool).u32(ppixels.len() as u32)); // create_pool
    c.conn.queue_fd(pmfd);
    pump!();
    unsafe { libc::close(pmfd) };
    let pbuffer = c.alloc();
    c.conn.send(
        &Message::new(ppool, 0)
            .u32(pbuffer)
            .i32(0)
            .i32(pw)
            .i32(ph)
            .i32(pw * 4)
            .u32(1), // XRGB8888
    );
    c.conn.send(&Message::new(psurface, 1).u32(pbuffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(psurface, 6)); // commit → present popup as its own window
    pump!();

    // The popup reached the Presenter as its OWN SurfaceBuffer (sid == the popup surface) carrying a
    // placement that resolves to the anchor: the DIRECT parent surface (the toplevel) + the positioner
    // geometry origin (10, 54). This is the native-window parity the legacy path gets from
    // SurfaceBuffer::popup — a menu is no longer clipped to the toplevel frame.
    let (sid, placement) = last_popup
        .lock()
        .unwrap()
        .clone()
        .expect("the popup's committed frame should have been presented as its own window");
    assert_eq!(sid, psurface, "the presented popup window should be keyed by the popup's surface id");
    let placement = placement.expect("the popup SurfaceBuffer must carry a PopupPlacement");
    assert_eq!(
        placement.parent_sid, surface,
        "the popup should anchor to its direct parent surface (the toplevel)"
    );
    assert_eq!(
        (placement.x, placement.y),
        (10, 54),
        "the popup placement should resolve to the positioner geometry origin"
    );
}
