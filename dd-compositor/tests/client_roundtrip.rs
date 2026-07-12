//! Real-client integration proof for the Smithay-native compositor. A minimal in-process Wayland
//! client (built on `dd_display::wire`) connects over a `socketpair` that we hand to Smithay's
//! `Display` as a client, drives `get_registry` + the `xdg_shell`/`wl_shm` handshake, backs a pool
//! with a real `memfd`, and commits a frame. We assert: (1) every parity global is advertised, and
//! (2) the commit reaches the `Presenter` (frame_count advances) — i.e. the guest→host present path
//! works end to end through the Smithay core. This mirrors `dd-display`'s headless `server.rs` test,
//! but against `DdState`, so both protocol machines are proven by the same kind of client.
//!
//! Runs headlessly on Linux (libxkbcommon present) and on macOS.

use dd_compositor::{ClientState, DdState};
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::os::unix::io::RawFd;
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;

struct Client {
    conn: Conn,
    next_id: u32,
    globals: std::collections::HashMap<String, (u32, u32)>,
    /// The client's `wl_pointer` id, so `drain` can capture the `enter` serial (needed to satisfy the
    /// serial check on `wp_cursor_shape_device_v1.set_shape`). 0 = no pointer bound yet.
    pointer_id: u32,
    /// Serial from the last `wl_pointer.enter` (opcode 0) — echoed back in `set_shape`.
    enter_serial: Option<u32>,
    /// The client's `xdg_surface` id, so `drain` can capture the configure serial for the ack handshake.
    xdg_id: u32,
    /// The client's `xdg_toplevel` id, so `drain` can decode `configure(w,h,states)`.
    toplevel_id: u32,
    /// Serial from the last `xdg_surface.configure` (opcode 0) — echoed back in `ack_configure`.
    last_xdg_configure_serial: Option<u32>,
    /// Decoded `(width, height, states)` from the last `xdg_toplevel.configure` (opcode 0).
    last_toplevel_configure: Option<(i32, i32, Vec<u32>)>,
    /// Every event seen as `(object, opcode)`, so tests can assert e.g. a `presented` (feedback opcode 1).
    events: Vec<(u32, u16)>,
}

impl Client {
    fn new(fd: RawFd) -> Client {
        Client {
            conn: Conn::new(fd),
            next_id: 2,
            globals: Default::default(),
            pointer_id: 0,
            enter_serial: None,
            xdg_id: 0,
            toplevel_id: 0,
            last_xdg_configure_serial: None,
            last_toplevel_configure: None,
            events: Vec::new(),
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
            self.events.push((m.object, m.opcode));
            // wl_registry.global(name, iface, version): registry is id 2 in this tiny client.
            if m.opcode == 0 && m.object == 2 {
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            } else if self.pointer_id != 0 && m.object == self.pointer_id && m.opcode == 0 {
                // wl_pointer.enter(serial, surface, x, y): first arg is the serial.
                self.enter_serial = Some(m.reader().u32());
            } else if self.xdg_id != 0 && m.object == self.xdg_id && m.opcode == 0 {
                // xdg_surface.configure(serial): the serial the client must echo in ack_configure.
                self.last_xdg_configure_serial = Some(m.reader().u32());
            } else if self.toplevel_id != 0 && m.object == self.toplevel_id && m.opcode == 0 {
                // xdg_toplevel.configure(width, height, states[]): the compositor's size + state hint.
                let mut r = m.reader();
                let w = r.i32();
                let h = r.i32();
                let states = r
                    .array()
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                self.last_toplevel_configure = Some((w, h, states));
            }
        }
    }
    fn saw(&self, object: u32, opcode: u16) -> bool {
        self.events.contains(&(object, opcode))
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

// NOTE: this is intentionally ONE test driving ONE `Display`/client. `wayland-server`'s `Display` +
// client machinery keeps process-global state, so two client-carrying `Display`s in the same test
// binary interfere (the second's socket is torn down mid-handshake). Each concern below passes in
// isolation; folding them into a single connection keeps the whole proof deterministic.
#[test]
fn globals_advertise_frame_presents_feedback_and_cursor_shape_wire() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let last_shape = Arc::new(Mutex::new(None));
    let mut state = DdState::new(
        dh.clone(),
        Box::new(RecordingPresenter {
            frames: 0,
            last_shape: last_shape.clone(),
        }),
    );

    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();

    let mut c = Client::new(client_fd);

    // pump(): flush client → dispatch server → flush server → drain client.
    macro_rules! pump {
        () => {{
            c.flush();
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }};
    }

    // get_registry → the compositor advertises all globals.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();

    for iface in [
        "wl_compositor",
        "wl_subcompositor",
        "wl_shm",
        "xdg_wm_base",
        "wl_seat",
        "wl_output",
        "wp_viewporter",
        "wp_presentation",
        "wp_cursor_shape_manager_v1",
    ] {
        assert!(
            c.globals.contains_key(iface),
            "global {iface} not advertised; got {:?}",
            c.globals.keys().collect::<Vec<_>>()
        );
    }
    // Smithay advertises wl_seat at its max (v9) — a superset of server.rs's v5 (clients bind at the
    // lower of the two), so parity is "at least v5". wl_output is v4 (name/description), matching.
    assert!(c.globals["wl_seat"].1 >= 5, "wl_seat >= v5 (got v{})", c.globals["wl_seat"].1);
    assert!(c.globals["wl_output"].1 >= 4, "wl_output >= v4 (got v{})", c.globals["wl_output"].1);

    // Bind compositor + shm + xdg_wm_base.
    let bind = |c: &mut Client, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };
    let comp = bind(&mut c, "wl_compositor", 4);
    let shm = bind(&mut c, "wl_shm", 1);
    let wm = bind(&mut c, "xdg_wm_base", 1);
    let seat = bind(&mut c, "wl_seat", 5);
    let presentation = bind(&mut c, "wp_presentation", 1);
    let cursor_mgr = bind(&mut c, "wp_cursor_shape_manager_v1", 1);

    // wl_seat.get_pointer(0): track the id so drain captures the wl_pointer.enter serial (needed to
    // satisfy the serial check on wp_cursor_shape_device_v1.set_shape).
    let pointer = c.alloc();
    c.conn.send(&Message::new(seat, 0).u32(pointer));
    c.pointer_id = pointer;

    // surface + xdg toplevel + initial commit → configure.
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    let xdg = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    let toplevel = c.alloc();
    c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.xdg_id = xdg;
    c.toplevel_id = toplevel;
    c.conn.send(&Message::new(surface, 6)); // commit (no buffer)
    pump!();

    // The xdg-shell configure handshake: mapping the toplevel MUST produce a paired
    // xdg_toplevel.configure(w,h,states) + xdg_surface.configure(serial). Assert both arrived, that the
    // initial configure carries the floating size (1000x700) with the Activated state (4), and that the
    // client can ack the serial the compositor actually chose (not a hardcoded 1).
    assert!(c.saw(toplevel, 0), "expected xdg_toplevel.configure; saw {:?}", c.events);
    assert!(c.saw(xdg, 0), "expected xdg_surface.configure; saw {:?}", c.events);
    let (cw, ch, cstates) = c
        .last_toplevel_configure
        .clone()
        .expect("initial xdg_toplevel.configure not decoded");
    assert_eq!((cw, ch), (1000, 700), "initial configure size");
    assert!(cstates.contains(&4), "initial configure should carry Activated (4); got {cstates:?}");
    let init_serial = c
        .last_xdg_configure_serial
        .expect("initial xdg_surface.configure serial not captured");
    c.conn.send(&Message::new(xdg, 4).u32(init_serial)); // ack_configure(serial)

    // Back a 4x3 XRGB buffer with a portable anonymous shm fd (memfd on Linux, shm/tmpfile on macOS),
    // prefilled with a recognizable BGRA pattern. Reuses dd-display's `keymap::anon_fd_with`.
    let (w, h): (i32, i32) = (4, 3);
    let stride = w * 4;
    let size = (stride * h) as usize;
    let mut pixels = vec![0u8; size];
    for i in 0..(size / 4) {
        pixels[i * 4] = 0x20; // B
        pixels[i * 4 + 1] = 0x40; // G
        pixels[i * 4 + 2] = 0xC8; // R
        pixels[i * 4 + 3] = 0x00; // X
    }
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
            .u32(1), // wl_shm format XRGB8888 == 1
    );
    // Request wp_presentation feedback for the NEXT content update (the commit below); Chrome/viz waits
    // on this to keep its frame clock ticking.
    let feedback = c.alloc();
    c.conn
        .send(&Message::new(presentation, 1).u32(surface).u32(feedback)); // wp_presentation.feedback
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(surface, 2).i32(0).i32(0).i32(w).i32(h)); // damage
    c.conn.send(&Message::new(surface, 6)); // commit → present + feedback answered
    pump!();

    assert!(
        state.presenter.frame_count() >= 1,
        "the committed frame did not reach the presenter"
    );
    // (1) wp_presentation feedback: a successful present must answer with `presented` (opcode 1).
    assert!(
        c.saw(feedback, 1),
        "expected wp_presentation_feedback.presented (opcode 1) on {feedback}; saw {:?}",
        c.events
    );

    // (2) wp_cursor_shape wiring: give the pointer focus (in-process) so set_shape's serial check
    // passes, read the enter serial, then drive set_shape(pointer=4) and assert the Presenter received
    // the correctly-mapped wp_cursor_shape enum number (NOT the CursorIcon discriminant).
    state.pointer_motion(2.0, 2.0);
    display.flush_clients().unwrap();
    c.drain();
    let serial = c
        .enter_serial
        .expect("wl_pointer.enter should have delivered a serial");
    let device = c.alloc();
    c.conn
        .send(&Message::new(cursor_mgr, 1).u32(device).u32(pointer)); // manager.get_pointer(device, pointer)
    c.conn.send(&Message::new(device, 1).u32(serial).u32(4)); // device.set_shape(serial, pointer=4)
    pump!();

    assert_eq!(
        *last_shape.lock().unwrap(),
        Some(4),
        "set_shape(pointer=4) should reach the Presenter as wp_cursor_shape enum 4"
    );

    // (3) Window management: a host-driven window resize reconfigures the focused toplevel. Drive
    // `resize_focused` (what the macOS loop's `maybe_resize_focused` calls when the user drags the
    // window edge) and assert the client receives a fresh configure carrying the new size + Activated,
    // then completes the ack handshake with the compositor's new serial.
    c.last_toplevel_configure = None;
    state.resize_focused(1280, 720);
    display.flush_clients().unwrap();
    c.drain();
    let (rw, rh, rstates) = c
        .last_toplevel_configure
        .clone()
        .expect("resize did not produce an xdg_toplevel.configure");
    assert_eq!((rw, rh), (1280, 720), "resize_focused should configure the requested size");
    assert!(rstates.contains(&4), "resize configure should carry Activated (4); got {rstates:?}");
    let rserial = c
        .last_xdg_configure_serial
        .expect("resize xdg_surface.configure serial not captured");
    c.conn.send(&Message::new(xdg, 4).u32(rserial)); // ack_configure(resize serial)
    pump!();

    // (4) min/max-size clamping: the client sets a max size (double-buffered — applied on the next
    // commit), after which a larger host resize is clamped down to it, honouring set_max_size.
    c.conn.send(&Message::new(toplevel, 7).i32(900).i32(600)); // xdg_toplevel.set_max_size(900,600)
    c.conn.send(&Message::new(surface, 6)); // commit → apply cached max_size
    pump!();
    c.last_toplevel_configure = None;
    state.resize_focused(2000, 2000);
    display.flush_clients().unwrap();
    c.drain();
    let (mw, mh, _) = c
        .last_toplevel_configure
        .clone()
        .expect("clamped resize did not produce a configure");
    assert_eq!(
        (mw, mh),
        (900, 600),
        "resize beyond set_max_size(900,600) should clamp to the max"
    );
}

/// A `Presenter` that records the last `set_cursor_shape` (so the cursor-shape wiring can be asserted)
/// and always reports a successful present (so `wp_presentation` feedback resolves to `presented`).
struct RecordingPresenter {
    frames: u32,
    /// Shared with the test: the last `wp_cursor_shape_device_v1.shape` value the compositor mapped.
    last_shape: Arc<Mutex<Option<u32>>>,
}
impl dd_display::present::Presenter for RecordingPresenter {
    fn present(&mut self, _surf: &dd_display::present::SurfaceBuffer) -> bool {
        self.frames += 1;
        true
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
    fn set_cursor_shape(&self, shape: u32) {
        *self.last_shape.lock().unwrap() = Some(shape);
    }
}

use std::os::unix::io::FromRawFd;
use std::sync::Mutex;
