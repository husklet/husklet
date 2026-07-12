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
    /// Serial from the last `wl_pointer.button` (opcode 3) — a valid grab serial for move/resize.
    button_serial: Option<u32>,
    /// The client's `xdg_surface` id, so `drain` can capture the configure serial for the ack handshake.
    xdg_id: u32,
    /// The client's `xdg_toplevel` id, so `drain` can decode `configure(w,h,states)`.
    toplevel_id: u32,
    /// Serial from the last `xdg_surface.configure` (opcode 0) — echoed back in `ack_configure`.
    last_xdg_configure_serial: Option<u32>,
    /// Decoded `(width, height, states)` from the last `xdg_toplevel.configure` (opcode 0).
    last_toplevel_configure: Option<(i32, i32, Vec<u32>)>,
    /// Every `xdg_toplevel.configure` state array seen, so a test can assert a transient state (e.g.
    /// `Resizing`) appeared even after a later configure cleared it.
    toplevel_configure_states: Vec<Vec<u32>>,
    /// The client's `zxdg_toplevel_decoration_v1` id, so `drain` can decode `configure(mode)`. 0 = none.
    deco_id: u32,
    /// The mode (1=client_side, 2=server_side) from the last decoration `configure` (opcode 0).
    last_deco_mode: Option<u32>,
    /// The client's `xdg_activation_token_v1` id, so `drain` can decode `done(token)`. 0 = none.
    act_token_id: u32,
    /// The token string from `xdg_activation_token_v1.done` (opcode 0).
    act_token: Option<String>,
    /// The client's `xdg_popup` id, so `drain` can decode `configure(x,y,w,h)` (opcode 0).
    popup_id: u32,
    /// The popup's `xdg_surface` id, so `drain` can capture its configure serial for the ack handshake.
    popup_xdg_id: u32,
    /// Decoded `(x, y, w, h)` from the last `xdg_popup.configure` (opcode 0) — the positioner placement.
    last_popup_configure: Option<(i32, i32, i32, i32)>,
    /// Serial from the last `xdg_surface.configure` (opcode 0) on the popup's xdg_surface.
    last_popup_xdg_serial: Option<u32>,
    /// The client's `wl_keyboard` id, so `drain` can decode `enter`/`modifiers`. 0 = not bound.
    keyboard_id: u32,
    /// Serial from the last `wl_keyboard.enter` (opcode 1) — a valid serial for `set_selection`.
    kbd_enter_serial: Option<u32>,
    /// Decoded `(depressed, latched, locked, group)` from the last `wl_keyboard.modifiers` (opcode 4).
    last_modifiers: Option<(u32, u32, u32, u32)>,
    /// The client's `wl_data_device` id, so `drain` can decode `data_offer`/`selection`. 0 = not bound.
    data_device_id: u32,
    /// The server-allocated `wl_data_offer` id from the last `wl_data_device.data_offer` (opcode 0).
    last_data_offer: Option<u32>,
    /// Whether a `wl_data_device.selection` (opcode 5) pointed at `last_data_offer` (vs. cleared to null).
    selection_offer: Option<u32>,
    /// Mime types the last `wl_data_offer` announced via `wl_data_offer.offer` (opcode 0).
    offer_mimes: Vec<String>,
    /// The client's `zwp_primary_selection_device_v1` id, so `drain` can decode its `data_offer`/`selection`.
    primary_device_id: u32,
    /// The server-allocated `zwp_primary_selection_offer_v1` id from the last primary `data_offer` (opcode 0).
    last_primary_offer: Option<u32>,
    /// Whether the last primary `selection` (opcode 1) pointed at an offer (vs. cleared to null).
    primary_selection_offer: Option<u32>,
    /// Mime types the last primary offer announced via `zwp_primary_selection_offer_v1.offer` (opcode 0).
    primary_offer_mimes: Vec<String>,
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
            button_serial: None,
            xdg_id: 0,
            toplevel_id: 0,
            last_xdg_configure_serial: None,
            last_toplevel_configure: None,
            toplevel_configure_states: Vec::new(),
            deco_id: 0,
            last_deco_mode: None,
            act_token_id: 0,
            act_token: None,
            popup_id: 0,
            popup_xdg_id: 0,
            last_popup_configure: None,
            last_popup_xdg_serial: None,
            keyboard_id: 0,
            kbd_enter_serial: None,
            last_modifiers: None,
            data_device_id: 0,
            last_data_offer: None,
            selection_offer: None,
            offer_mimes: Vec::new(),
            primary_device_id: 0,
            last_primary_offer: None,
            primary_selection_offer: None,
            primary_offer_mimes: Vec::new(),
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
            } else if self.pointer_id != 0 && m.object == self.pointer_id && m.opcode == 3 {
                // wl_pointer.button(serial, time, button, state): first arg is the grab serial.
                self.button_serial = Some(m.reader().u32());
            } else if self.deco_id != 0 && m.object == self.deco_id && m.opcode == 0 {
                // zxdg_toplevel_decoration_v1.configure(mode): the negotiated decoration mode.
                self.last_deco_mode = Some(m.reader().u32());
            } else if self.act_token_id != 0 && m.object == self.act_token_id && m.opcode == 0 {
                // xdg_activation_token_v1.done(token): the minted activation token string.
                self.act_token = Some(m.reader().string());
            } else if self.xdg_id != 0 && m.object == self.xdg_id && m.opcode == 0 {
                // xdg_surface.configure(serial): the serial the client must echo in ack_configure.
                self.last_xdg_configure_serial = Some(m.reader().u32());
            } else if self.toplevel_id != 0 && m.object == self.toplevel_id && m.opcode == 0 {
                // xdg_toplevel.configure(width, height, states[]): the compositor's size + state hint.
                let mut r = m.reader();
                let w = r.i32();
                let h = r.i32();
                let states: Vec<u32> = r
                    .array()
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                self.toplevel_configure_states.push(states.clone());
                self.last_toplevel_configure = Some((w, h, states));
            } else if self.popup_id != 0 && m.object == self.popup_id && m.opcode == 0 {
                // xdg_popup.configure(x, y, w, h): the placement the positioner resolved to.
                let mut r = m.reader();
                let x = r.i32();
                let y = r.i32();
                let w = r.i32();
                let h = r.i32();
                self.last_popup_configure = Some((x, y, w, h));
            } else if self.popup_xdg_id != 0 && m.object == self.popup_xdg_id && m.opcode == 0 {
                // The popup's xdg_surface.configure(serial) — echoed back in ack_configure.
                self.last_popup_xdg_serial = Some(m.reader().u32());
            } else if self.keyboard_id != 0 && m.object == self.keyboard_id {
                match m.opcode {
                    // wl_keyboard.enter(serial, surface, keys[]): capture the serial for set_selection.
                    1 => self.kbd_enter_serial = Some(m.reader().u32()),
                    // wl_keyboard.modifiers(serial, depressed, latched, locked, group).
                    4 => {
                        let mut r = m.reader();
                        let _serial = r.u32();
                        self.last_modifiers = Some((r.u32(), r.u32(), r.u32(), r.u32()));
                    }
                    _ => {}
                }
            } else if self.data_device_id != 0 && m.object == self.data_device_id {
                match m.opcode {
                    // wl_data_device.data_offer(new_id): the server-allocated offer proxy id.
                    0 => self.last_data_offer = Some(m.reader().u32()),
                    // wl_data_device.selection(object? id): the current selection offer (0 == cleared).
                    5 => {
                        let id = m.reader().u32();
                        self.selection_offer = if id == 0 { None } else { Some(id) };
                    }
                    _ => {}
                }
            } else if self.last_data_offer == Some(m.object) && m.opcode == 0 {
                // wl_data_offer.offer(mime_type): a mime the current selection offers.
                self.offer_mimes.push(m.reader().string());
            } else if self.primary_device_id != 0 && m.object == self.primary_device_id {
                match m.opcode {
                    // zwp_primary_selection_device_v1.data_offer(new_id): the server-allocated offer proxy.
                    0 => self.last_primary_offer = Some(m.reader().u32()),
                    // zwp_primary_selection_device_v1.selection(object? id): current primary selection.
                    1 => {
                        let id = m.reader().u32();
                        self.primary_selection_offer = if id == 0 { None } else { Some(id) };
                    }
                    _ => {}
                }
            } else if self.last_primary_offer == Some(m.object) && m.opcode == 0 {
                // zwp_primary_selection_offer_v1.offer(mime_type): a mime the primary selection offers.
                self.primary_offer_mimes.push(m.reader().string());
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
    let host = Arc::new(Mutex::new(None));
    let set_host = Arc::new(Mutex::new(None));
    let last_move = Arc::new(Mutex::new(None));
    let last_resize = Arc::new(Mutex::new(None));
    let last_raise = Arc::new(Mutex::new(None));
    let last_cursor_buffer = Arc::new(Mutex::new(None));
    let cursor_hidden = Arc::new(Mutex::new(None));
    let mut state = DdState::new(
        dh.clone(),
        Box::new(RecordingPresenter {
            frames: 0,
            last_shape: last_shape.clone(),
            host: host.clone(),
            set_host: set_host.clone(),
            last_move: last_move.clone(),
            last_resize: last_resize.clone(),
            last_raise: last_raise.clone(),
            last_cursor_buffer: last_cursor_buffer.clone(),
            cursor_hidden: cursor_hidden.clone(),
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
        "wl_data_device_manager",
        "zxdg_decoration_manager_v1",
        "xdg_activation_v1",
        "zwp_primary_selection_device_manager_v1",
        "zwp_relative_pointer_manager_v1",
        "zwp_pointer_constraints_v1",
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
    let ddm = bind(&mut c, "wl_data_device_manager", 3);

    // wl_seat.get_pointer(0): track the id so drain captures the wl_pointer.enter serial (needed to
    // satisfy the serial check on wp_cursor_shape_device_v1.set_shape).
    let pointer = c.alloc();
    c.conn.send(&Message::new(seat, 0).u32(pointer));
    c.pointer_id = pointer;

    // wl_seat.get_keyboard(1): bound BEFORE the toplevel maps so focus delivers keymap+enter to it, then
    // wl_keyboard.modifiers when the host modifier state changes.
    let keyboard = c.alloc();
    c.conn.send(&Message::new(seat, 1).u32(keyboard));
    c.keyboard_id = keyboard;

    // wl_data_device_manager.get_data_device(1, id, seat): the per-seat clipboard/DnD endpoint. Bound
    // before focus so it becomes the focused client's data device (set_data_device_focus follows kbd focus).
    let data_device = c.alloc();
    c.conn.send(&Message::new(ddm, 1).u32(data_device).u32(seat));
    c.data_device_id = data_device;

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

    // (5) xdg_popup round-trip: map → configure → present → reposition → grab → dismiss. This is the
    // Chrome menu/dropdown/tooltip path. Bind xdg_wm_base at v3 so xdg_popup.reposition + the
    // xdg_popup.repositioned event are available.
    let wm3 = bind(&mut c, "xdg_wm_base", 3);

    // Positioner: a 120x80 popup anchored at the bottom-left corner of a 200x24 anchor rectangle placed at
    // (10,30) in the parent's window geometry, growing toward the bottom-right. With no constraint kicking
    // in (the popup fits the output), this resolves to geometry (10, 54, 120, 80): anchor point =
    // (10, 30+24), bottom_right gravity subtracts nothing.
    let pos = c.alloc();
    c.conn.send(&Message::new(wm3, 1).u32(pos)); // create_positioner
    c.conn.send(&Message::new(pos, 1).i32(120).i32(80)); // set_size(w,h)
    c.conn
        .send(&Message::new(pos, 2).i32(10).i32(30).i32(200).i32(24)); // set_anchor_rect(x,y,w,h)
    c.conn.send(&Message::new(pos, 3).u32(6)); // set_anchor(bottom_left = 6)
    c.conn.send(&Message::new(pos, 4).u32(8)); // set_gravity(bottom_right = 8)

    // Popup surface + its xdg_surface + get_popup(parent = the toplevel's xdg_surface, positioner).
    let psurface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(psurface)); // create_surface
    let pxdg = c.alloc();
    c.conn.send(&Message::new(wm3, 2).u32(pxdg).u32(psurface)); // get_xdg_surface
    let popup = c.alloc();
    c.conn
        .send(&Message::new(pxdg, 2).u32(popup).u32(xdg).u32(pos)); // xdg_surface.get_popup(id, parent, positioner)
    c.popup_id = popup;
    c.popup_xdg_id = pxdg;
    pump!();

    // The initial handshake: xdg_popup.configure(x,y,w,h) paired with the popup's xdg_surface.configure.
    assert!(c.saw(popup, 0), "expected xdg_popup.configure; saw {:?}", c.events);
    assert!(c.saw(pxdg, 0), "expected popup xdg_surface.configure; saw {:?}", c.events);
    assert_eq!(
        c.last_popup_configure,
        Some((10, 54, 120, 80)),
        "positioner should resolve to (10,54,120,80)"
    );
    let pserial = c
        .last_popup_xdg_serial
        .expect("popup xdg_surface.configure serial not captured");
    c.conn.send(&Message::new(pxdg, 4).u32(pserial)); // ack_configure(serial)

    // Back the popup with a small buffer and commit → it composites into the owning toplevel window (the
    // present path re-presents the toplevel with the popup blended at its offset).
    let frames_before_popup = state.presenter.frame_count();
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
            .u32(1),
    ); // create_buffer (XRGB8888)
    c.conn.send(&Message::new(psurface, 1).u32(pbuffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(psurface, 6)); // commit
    pump!();
    assert!(
        state.presenter.frame_count() > frames_before_popup,
        "committing the popup buffer should re-present the composited toplevel"
    );

    // reposition (v3): move the popup to a new anchor rect at (50,60). The compositor answers with
    // xdg_popup.repositioned(token) (opcode 2) followed by a fresh configure at (50,84,120,80).
    let pos2 = c.alloc();
    c.conn.send(&Message::new(wm3, 1).u32(pos2)); // create_positioner
    c.conn.send(&Message::new(pos2, 1).i32(120).i32(80)); // set_size
    c.conn
        .send(&Message::new(pos2, 2).i32(50).i32(60).i32(200).i32(24)); // set_anchor_rect
    c.conn.send(&Message::new(pos2, 3).u32(6)); // set_anchor(bottom_left)
    c.conn.send(&Message::new(pos2, 4).u32(8)); // set_gravity(bottom_right)
    c.last_popup_configure = None;
    c.conn.send(&Message::new(popup, 2).u32(pos2).u32(77)); // xdg_popup.reposition(positioner, token=77)
    pump!();
    assert!(
        c.saw(popup, 2),
        "expected xdg_popup.repositioned (opcode 2); saw {:?}",
        c.events
    );
    assert_eq!(
        c.last_popup_configure,
        Some((50, 84, 120, 80)),
        "reposition should re-place the popup to (50,84,120,80)"
    );
    let pserial2 = c
        .last_popup_xdg_serial
        .expect("reposition xdg_surface.configure serial not captured");
    c.conn.send(&Message::new(pxdg, 4).u32(pserial2)); // ack the reposition configure

    // grab(seat, serial): an explicit popup grab (the compositor tracks it so a click-outside dismisses
    // the whole chain). Reuse the pointer-enter serial captured earlier.
    let gserial = c.enter_serial.expect("need an input serial for the popup grab");
    c.conn.send(&Message::new(popup, 1).u32(seat).u32(gserial)); // xdg_popup.grab(seat, serial)
    pump!();

    // Dismissal: the input loop calls dismiss_popup_grabs() on a click outside the grab. The client MUST
    // receive xdg_popup.popup_done (opcode 1) so the menu closes.
    let dismissed = state.dismiss_popup_grabs();
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(dismissed, 1, "the grabbing popup should be dismissed");
    assert!(
        c.saw(popup, 1),
        "expected xdg_popup.popup_done (opcode 1) on dismissal; saw {:?}",
        c.events
    );

    // (6) Keyboard modifiers: the toplevel has keyboard focus (from mapping), so the wl_keyboard bound
    // above received keymap + enter. Driving the host modifier state (Shift) must emit a
    // wl_keyboard.modifiers whose `depressed` mask carries the Shift bit — this is the FlagsChanged path
    // the macOS loop feeds via `update_modifiers`.
    assert!(c.saw(keyboard, 1), "wl_keyboard.enter expected on focus; saw {:?}", c.events);
    c.last_modifiers = None;
    state.update_modifiers(0b0_0001); // Shift down
    display.flush_clients().unwrap();
    c.drain();
    let (dep, _lat, _lock, _grp) = c
        .last_modifiers
        .expect("update_modifiers should emit wl_keyboard.modifiers");
    assert!(dep & 0x1 != 0, "Shift should set the depressed Shift bit; got depressed={dep:#x}");
    // Releasing all modifiers clears the depressed mask.
    c.last_modifiers = None;
    state.update_modifiers(0);
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(
        c.last_modifiers.map(|m| m.0),
        Some(0),
        "clearing modifiers should zero the depressed mask"
    );

    // (7) Clipboard host→guest (paste): the host clipboard changes, so `offer_host_clipboard` advertises it
    // to the focused guest as a compositor selection (data_offer + offer(mime) + selection). A guest paste
    // (wl_data_offer.receive) is then answered from the host clipboard bytes via `send_selection` — proven
    // end to end by round-tripping the payload back through a pipe.
    let mime = "text/plain;charset=utf-8";
    *host.lock().unwrap() = Some((mime.to_string(), b"hello-from-host".to_vec(), 1));
    c.last_data_offer = None;
    c.offer_mimes.clear();
    state.offer_host_clipboard();
    display.flush_clients().unwrap();
    c.drain();
    let offer = c
        .last_data_offer
        .expect("host clipboard should have produced a wl_data_device.data_offer");
    assert!(
        c.offer_mimes.iter().any(|m| m == mime),
        "the host selection should offer {mime}; got {:?}",
        c.offer_mimes
    );
    assert_eq!(
        c.selection_offer,
        Some(offer),
        "wl_data_device.selection should point at the host offer"
    );
    // Paste: hand the compositor a pipe write-end via wl_data_offer.receive(mime, fd); read it back.
    let mut pfds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(pfds.as_mut_ptr()) }, 0);
    let (rd, wr) = (pfds[0], pfds[1]);
    c.conn.send(&Message::new(offer, 1).string(mime)); // wl_data_offer.receive(mime, fd)
    c.conn.queue_fd(wr);
    pump!();
    unsafe { libc::close(wr) }; // drop our write end so the read sees EOF after the compositor's bytes
    let mut buf = [0u8; 64];
    let n = unsafe { libc::read(rd, buf.as_mut_ptr() as *mut _, buf.len()) };
    unsafe { libc::close(rd) };
    assert!(n > 0, "paste read returned {n}");
    assert_eq!(
        &buf[..n as usize],
        b"hello-from-host",
        "the guest paste should read the host clipboard bytes back"
    );

    // (8) Clipboard guest→host (copy): the focused guest sets its own selection from a wl_data_source. The
    // compositor's `new_selection` captures the offered mime types for export to the host clipboard (the
    // runtime loop then reads the source and calls `clipboard_set_host`).
    assert!(state.pending_host_copy().is_none(), "no export should be pending yet");
    let source = c.alloc();
    c.conn.send(&Message::new(ddm, 0).u32(source)); // create_data_source(id)
    let copy_mime = "text/plain";
    c.conn.send(&Message::new(source, 0).string(copy_mime)); // wl_data_source.offer(mime)
    let sel_serial = c.kbd_enter_serial.expect("need a keyboard enter serial for set_selection");
    c.conn
        .send(&Message::new(data_device, 1).u32(source).u32(sel_serial)); // set_selection(source, serial)
    pump!();
    assert_eq!(
        state.pending_host_copy(),
        Some(&[copy_mime.to_string()][..]),
        "a guest copy should queue its mime types for export to the host clipboard"
    );

    // (9) Window-management UX: server-side-decoration negotiation, interactive move/resize grabs (serial
    // validated against a real input event), and xdg-activation focus/raise. This is what GTK/Qt/SDL need
    // to be usable windows.

    // Focus the pointer on the toplevel and press the left button so the client captures a REAL grab serial
    // (the implicit pointer-button grab). The compositor recorded this serial via note_input_serial, so a
    // move/resize echoing it validates; a bogus serial does not.
    state.pointer_motion(10.0, 10.0);
    state.pointer_button(0x110, true); // BTN_LEFT down
    display.flush_clients().unwrap();
    c.drain();
    let grab_serial = c
        .button_serial
        .expect("a button press should deliver a wl_pointer.button grab serial");

    // (9a) zxdg_decoration_manager_v1. Enable the native-title-bar policy so BOTH modes are reachable and
    // distinguishable (otherwise the borderless-host policy always answers CSD, and Smithay never re-sends
    // an unchanged mode, so a set_mode round-trip would be unobservable). With SSD available: new_decoration
    // defaults to server_side; set_mode(client_side) is honoured as CSD; set_mode(server_side) as SSD.
    std::env::set_var("DD_DISPLAY_WINDOW_DECORATIONS", "1");
    let deco_mgr = bind(&mut c, "zxdg_decoration_manager_v1", 1);
    let deco = c.alloc();
    c.conn
        .send(&Message::new(deco_mgr, 1).u32(deco).u32(toplevel)); // get_toplevel_decoration(id, toplevel)
    c.deco_id = deco;
    pump!();
    assert_eq!(
        c.last_deco_mode,
        Some(2),
        "new_decoration should default to server_side when a native title bar is available"
    );
    c.last_deco_mode = None;
    c.conn.send(&Message::new(deco, 1).u32(1)); // set_mode(client_side = 1)
    pump!();
    assert_eq!(c.last_deco_mode, Some(1), "set_mode(client_side) should be honoured as CSD");
    c.last_deco_mode = None;
    c.conn.send(&Message::new(deco, 1).u32(2)); // set_mode(server_side = 2)
    pump!();
    assert_eq!(
        c.last_deco_mode,
        Some(2),
        "set_mode(server_side) should be honoured as SSD when a native title bar exists"
    );

    // (9b) xdg_toplevel.move: a valid grab serial reaches begin_interactive_move for the toplevel's surface.
    *last_move.lock().unwrap() = None;
    c.conn.send(&Message::new(toplevel, 5).u32(seat).u32(grab_serial)); // move(seat, serial)
    pump!();
    assert_eq!(
        *last_move.lock().unwrap(),
        Some(surface),
        "a serial-valid move grab should reach begin_interactive_move for the surface"
    );
    // A spoofed/stale serial must be rejected (no native drag started).
    *last_move.lock().unwrap() = None;
    c.conn
        .send(&Message::new(toplevel, 5).u32(seat).u32(0xDEAD_BEEF)); // move with a bogus serial
    pump!();
    assert_eq!(
        *last_move.lock().unwrap(),
        None,
        "a move grab with an unrecognised serial must be ignored"
    );

    // (9c) xdg_toplevel.resize(bottom_right): reaches begin_interactive_resize with the edge bitmask (10),
    // and the client first receives a configure carrying the Resizing (3) state.
    c.toplevel_configure_states.clear();
    *last_resize.lock().unwrap() = None;
    c.conn
        .send(&Message::new(toplevel, 6).u32(seat).u32(grab_serial).u32(10)); // resize(seat, serial, bottom_right=10)
    pump!();
    assert_eq!(
        *last_resize.lock().unwrap(),
        Some((surface, 10)),
        "a resize grab should reach begin_interactive_resize with the bottom_right edge bitmask"
    );
    assert!(
        c.toplevel_configure_states.iter().any(|s| s.contains(&3)),
        "an interactive resize should send a configure carrying the Resizing (3) state; saw {:?}",
        c.toplevel_configure_states
    );

    // (9d) xdg_activation_v1: mint a token (set_serial/set_surface/commit → done) then activate(token,
    // surface). The compositor focuses + raises the target window, so raise_window reaches the Presenter.
    let act = bind(&mut c, "xdg_activation_v1", 1);
    let token = c.alloc();
    c.conn.send(&Message::new(act, 1).u32(token)); // get_activation_token(id)
    c.act_token_id = token;
    c.conn.send(&Message::new(token, 0).u32(grab_serial).u32(seat)); // set_serial(serial, seat)
    c.conn.send(&Message::new(token, 2).u32(surface)); // set_surface(surface)
    c.conn.send(&Message::new(token, 3)); // commit
    pump!();
    let tok = c
        .act_token
        .clone()
        .expect("token commit should reply with xdg_activation_token_v1.done(token)");
    *last_raise.lock().unwrap() = None;
    c.conn.send(&Message::new(act, 2).string(&tok).u32(surface)); // activate(token, surface)
    pump!();
    assert_eq!(
        *last_raise.lock().unwrap(),
        Some(surface),
        "activating a surface should raise its window via the Presenter"
    );

    // (10) zwp_primary_selection_v1 (X11-style middle-click paste): the focused guest sets the PRIMARY
    // selection from its own source. Smithay offers it back to the focused primary device (data_offer +
    // offer(mime) + selection) — proving the guest↔guest wiring and that the primary selection follows
    // keyboard focus (set_primary_focus in focus_surface).
    let psm = bind(&mut c, "zwp_primary_selection_device_manager_v1", 1);
    let pdevice = c.alloc();
    c.conn.send(&Message::new(psm, 1).u32(pdevice).u32(seat)); // get_device(id, seat)
    c.primary_device_id = pdevice;
    pump!();
    let psource = c.alloc();
    c.conn.send(&Message::new(psm, 0).u32(psource)); // create_source(id)
    let pmime = "text/plain;charset=utf-8";
    c.conn.send(&Message::new(psource, 0).string(pmime)); // primary source.offer(mime)
    let pser = c.kbd_enter_serial.expect("need a serial for primary set_selection");
    c.last_primary_offer = None;
    c.primary_offer_mimes.clear();
    c.conn.send(&Message::new(pdevice, 0).u32(psource).u32(pser)); // device.set_selection(source, serial)
    pump!();
    let poffer = c
        .last_primary_offer
        .expect("setting the primary selection should offer it back to the focused primary device");
    assert!(
        c.primary_offer_mimes.iter().any(|m| m == pmime),
        "the primary offer should carry {pmime}; got {:?}",
        c.primary_offer_mimes
    );
    assert_eq!(
        c.primary_selection_offer,
        Some(poffer),
        "zwp_primary_selection_device_v1.selection should point at the primary offer"
    );

    // (10) zwp_relative_pointer_v1 (game/3D relative motion): bind the manager, get a relative pointer for
    // our wl_pointer, then feed a host delta and assert the client receives a relative_motion (opcode 0).
    let rpm = bind(&mut c, "zwp_relative_pointer_manager_v1", 1);
    let relptr = c.alloc();
    c.conn.send(&Message::new(rpm, 1).u32(relptr).u32(pointer)); // get_relative_pointer(id, pointer)
    pump!();
    state.relative_motion(5.0, -3.0);
    display.flush_clients().unwrap();
    c.drain();
    assert!(
        c.saw(relptr, 0),
        "expected zwp_relative_pointer_v1.relative_motion (opcode 0); saw {:?}",
        c.events
    );

    // (11) zwp_pointer_constraints_v1 (pointer lock — FPS mouselook): lock the pointer to the focused
    // surface. The compositor activates the lock (the surface holds focus), so the client receives
    // `locked` (opcode 0), `pointer_locked()` reports true, the host cursor is hidden, and absolute motion
    // is frozen while relative deltas keep flowing.
    *cursor_hidden.lock().unwrap() = None;
    let pcm = bind(&mut c, "zwp_pointer_constraints_v1", 1);
    let lock = c.alloc();
    // lock_pointer(id, surface, pointer, region=null, lifetime=persistent(2)).
    c.conn
        .send(&Message::new(pcm, 1).u32(lock).u32(surface).u32(pointer).u32(0).u32(2));
    pump!();
    assert!(
        c.saw(lock, 0),
        "an active lock should send zwp_locked_pointer_v1.locked (opcode 0); saw {:?}",
        c.events
    );
    assert!(state.pointer_locked(), "the pointer should report locked");
    assert_eq!(
        *cursor_hidden.lock().unwrap(),
        Some(true),
        "an active pointer lock should hide the host cursor"
    );
    // Absolute motion is frozen while locked (dropped); the relative stream still delivers.
    state.pointer_motion(999.0, 999.0);
    state.relative_motion(2.0, 2.0);
    display.flush_clients().unwrap();
    c.drain();
    // Release the lock so the following custom-cursor case starts from a visible pointer.
    c.conn.send(&Message::new(lock, 0)); // zwp_locked_pointer_v1.destroy
    pump!();
    assert!(!state.pointer_locked(), "destroying the lock should release it");

    // (12) wl_pointer.set_cursor client BITMAP cursor (CSS `cursor: url(...)`, a game crosshair, a custom
    // app cursor): the client assigns a cursor surface with a hotspot, then commits a buffer to it. The
    // compositor turns the committed pixels into a host cursor via the new Presenter hook — and MUST NOT
    // present the cursor surface as a window.
    let cur = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(cur)); // create_surface
    let cser = c.enter_serial.expect("need a pointer enter serial for set_cursor");
    // wl_pointer.set_cursor(serial, surface, hotspot_x, hotspot_y) — opcode 0.
    c.conn
        .send(&Message::new(pointer, 0).u32(cser).u32(cur).i32(2).i32(3));
    pump!();
    // Back the cursor with a 4x4 ARGB buffer (alpha honoured so cursor edges stay transparent).
    let (cw, ch): (i32, i32) = (4, 4);
    let csize = (cw * ch * 4) as usize;
    let cpixels = vec![0x77u8; csize];
    let cmfd = dd_display::keymap::anon_fd_with(&cpixels).expect("cursor anon shm fd");
    let cpool = c.alloc();
    c.conn.send(&Message::new(shm, 0).u32(cpool).u32(csize as u32)); // create_pool
    c.conn.queue_fd(cmfd);
    pump!();
    unsafe { libc::close(cmfd) };
    let cbuf = c.alloc();
    c.conn.send(
        &Message::new(cpool, 0)
            .u32(cbuf)
            .i32(0)
            .i32(cw)
            .i32(ch)
            .i32(cw * 4)
            .u32(0), // wl_shm format ARGB8888 == 0
    );
    c.conn.send(&Message::new(cur, 1).u32(cbuf).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(cur, 6)); // commit → build the host cursor (NOT a window)
    let frames_before_cursor = state.presenter.frame_count();
    pump!();
    let pushed = last_cursor_buffer
        .lock()
        .unwrap()
        .clone()
        .expect("a custom cursor surface commit should push a bitmap cursor to the Presenter");
    let (cbytes, bw, bh, bhx, bhy) = pushed;
    assert_eq!((bw, bh), (cw, ch), "cursor bitmap size should match the committed buffer");
    assert_eq!(
        (bhx, bhy),
        (2, 3),
        "the set_cursor hotspot should be forwarded (buffer_scale 1)"
    );
    assert_eq!(cbytes.len(), csize, "the cursor bitmap should carry the full BGRA buffer");
    assert_eq!(
        state.presenter.frame_count(),
        frames_before_cursor,
        "a cursor surface must NOT be presented as a window"
    );
}

/// A `Presenter` that records the last `set_cursor_shape` (so the cursor-shape wiring can be asserted)
/// and always reports a successful present (so `wp_presentation` feedback resolves to `presented`). It
/// also stands in for the host clipboard (`NSPasteboard`): `host` holds the host clipboard payload the
/// paste path serves, and `set_host` captures the guest→host copy the compositor exports.
struct RecordingPresenter {
    frames: u32,
    /// Shared with the test: the last `wp_cursor_shape_device_v1.shape` value the compositor mapped.
    last_shape: Arc<Mutex<Option<u32>>>,
    /// The simulated host clipboard: `(mime, bytes, generation)`. `generation` == the host `changeCount`.
    host: Arc<Mutex<Option<(String, Vec<u8>, u64)>>>,
    /// Captures the last `clipboard_set_host(mime, bytes)` — the guest→host copy the compositor exported.
    set_host: Arc<Mutex<Option<(String, Vec<u8>)>>>,
    /// The sid of the last `begin_interactive_move` (xdg_toplevel.move grab reaching the Presenter).
    last_move: Arc<Mutex<Option<u32>>>,
    /// The `(sid, edges)` of the last `begin_interactive_resize` (xdg_toplevel.resize grab).
    last_resize: Arc<Mutex<Option<(u32, u32)>>>,
    /// The sid of the last `raise_window` (an xdg_activation activation reaching the Presenter).
    last_raise: Arc<Mutex<Option<u32>>>,
    /// The last custom bitmap cursor the compositor pushed: `(bytes, width, height, hotspot_x, hotspot_y)`
    /// from `set_cursor_buffer` — so the `wl_pointer.set_cursor` client-bitmap path can be asserted.
    last_cursor_buffer: Arc<Mutex<Option<(Vec<u8>, i32, i32, i32, i32)>>>,
    /// The last `set_cursor_hidden` value (pointer hide/show for `set_cursor(null)` and pointer lock).
    cursor_hidden: Arc<Mutex<Option<bool>>>,
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
    fn set_cursor_buffer(&self, bgra: &[u8], w: i32, h: i32, hx: i32, hy: i32) {
        *self.last_cursor_buffer.lock().unwrap() = Some((bgra.to_vec(), w, h, hx, hy));
    }
    fn set_cursor_hidden(&self, hidden: bool) {
        *self.cursor_hidden.lock().unwrap() = Some(hidden);
    }
    fn clipboard_set_host(&self, mime: &str, bytes: &[u8]) {
        *self.set_host.lock().unwrap() = Some((mime.to_string(), bytes.to_vec()));
    }
    fn clipboard_host_mimes(&self) -> Vec<String> {
        match &*self.host.lock().unwrap() {
            Some((mime, _, _)) => vec![mime.clone()],
            None => Vec::new(),
        }
    }
    fn clipboard_host_read(&self, mime: &str) -> Option<Vec<u8>> {
        match &*self.host.lock().unwrap() {
            Some((m, bytes, _)) if m == mime => Some(bytes.clone()),
            _ => None,
        }
    }
    fn clipboard_host_generation(&self) -> u64 {
        self.host.lock().unwrap().as_ref().map(|(_, _, g)| *g).unwrap_or(0)
    }
    fn begin_interactive_move(&self, sid: u32) {
        *self.last_move.lock().unwrap() = Some(sid);
    }
    fn begin_interactive_resize(&self, sid: u32, edges: u32) {
        *self.last_resize.lock().unwrap() = Some((sid, edges));
    }
    fn raise_window(&self, sid: u32) {
        *self.last_raise.lock().unwrap() = Some(sid);
    }
}

use std::os::unix::io::FromRawFd;
use std::sync::Mutex;
