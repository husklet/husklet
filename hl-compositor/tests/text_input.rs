//! Integration proof for `zwp_text_input_v3` on the Smithay-native compositor.
//!
//! A minimal in-process Wayland client binds `zwp_text_input_manager_v3`, creates a text-input, maps a
//! toplevel (taking keyboard + text-input focus), and drives the enable/commit handshake. We then exercise
//! the host→guest IME seam the compositor exposes — `text_input_commit_string` / `preedit_string` /
//! `delete_surrounding` — and assert the client receives the matching `commit_string`/`preedit_string`/
//! `delete_surrounding_text` + `done` events. Finally we prove focus follows the keyboard (a second
//! toplevel re-homes text-input focus with `leave`+`enter`) and that a disabled field stops receiving text.
//!
//! Runs headlessly on Linux (libxkbcommon present) and macOS. One `Display`/client, per the note in
//! `client_roundtrip.rs` about wayland-server's process-global state.

use hl_compositor::{ClientState, DdState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;

/// The only `Presenter` method without a default is `present`; a headless counter is all these tests need.
struct CountingPresenter {
    frames: u32,
}
impl Presenter for CountingPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.frames += 1;
        Ok(PresentOutcome::Delivered { serial: self.frames as u64, timing: None })
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
}

/// A tiny Wayland client focused on the text-input surface: registry + a few binds, plus decoded
/// text-input events (enter/leave counts, the last commit_string / preedit / delete / done).
struct Cli {
    conn: Conn,
    next_id: u32,
    globals: HashMap<String, (u32, u32)>,
    ti_id: u32,
    enter_count: u32,
    leave_count: u32,
    last_commit_string: Option<String>,
    last_preedit: Option<(String, i32, i32)>,
    last_delete: Option<(u32, u32)>,
    last_done: Option<u32>,
    events: Vec<(u32, u16)>,
}

impl Cli {
    fn new(fd: RawFd) -> Cli {
        Cli {
            conn: Conn::new(fd),
            next_id: 2,
            globals: HashMap::new(),
            ti_id: 0,
            enter_count: 0,
            leave_count: 0,
            last_commit_string: None,
            last_preedit: None,
            last_delete: None,
            last_done: None,
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
            if m.opcode == 0 && m.object == 2 {
                // wl_registry.global(name, iface, version)
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            } else if self.ti_id != 0 && m.object == self.ti_id {
                match m.opcode {
                    0 => self.enter_count += 1,                 // enter(surface)
                    1 => self.leave_count += 1,                 // leave(surface)
                    2 => {
                        // preedit_string(text, cursor_begin, cursor_end)
                        let mut r = m.reader();
                        let text = r.string();
                        let cb = r.i32();
                        let ce = r.i32();
                        self.last_preedit = Some((text, cb, ce));
                    }
                    3 => self.last_commit_string = Some(m.reader().string()), // commit_string(text)
                    4 => {
                        // delete_surrounding_text(before_length, after_length)
                        let mut r = m.reader();
                        self.last_delete = Some((r.u32(), r.u32()));
                    }
                    5 => self.last_done = Some(m.reader().u32()), // done(serial)
                    _ => {}
                }
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

#[test]
fn text_input_focus_enable_and_commit_string_roundtrip() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(CountingPresenter { frames: 0 }));

    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();
    let mut c = Cli::new(client_fd);

    macro_rules! pump {
        () => {{
            c.flush();
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }};
    }

    // Registry: the text-input manager must be advertised (apps that bind it must find it).
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();
    assert!(
        c.globals.contains_key("zwp_text_input_manager_v3"),
        "zwp_text_input_manager_v3 must be advertised; got {:?}",
        c.globals.keys().collect::<Vec<_>>()
    );

    let bind = |c: &mut Cli, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };
    let comp = bind(&mut c, "wl_compositor", 4);
    let wm = bind(&mut c, "xdg_wm_base", 1);
    let seat = bind(&mut c, "wl_seat", 5);
    let ti_mgr = bind(&mut c, "zwp_text_input_manager_v3", 1);

    // get_text_input(id, seat) — opcode 1. Created BEFORE the surface maps: no focus yet ⇒ no enter.
    let ti = c.alloc();
    c.conn.send(&Message::new(ti_mgr, 1).u32(ti).u32(seat));
    c.ti_id = ti;
    pump!();
    assert_eq!(c.enter_count, 0, "text-input must not enter before its client is focused");
    assert!(
        !state.text_input_active(),
        "a text input with no focus/enable must not be active"
    );

    // Map a toplevel → keyboard focus → text-input enter follows.
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    let xdg = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    let toplevel = c.alloc();
    c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.conn.send(&Message::new(surface, 6)); // commit
    pump!();
    assert_eq!(c.enter_count, 1, "mapping+focus must deliver text-input enter; saw {:?}", c.events);
    assert!(c.saw(ti, 0), "expected zwp_text_input_v3.enter (opcode 0)");

    // Before enable, the field is not active and host text is not routed to it.
    assert!(!state.text_input_active(), "text input is not active until a committed enable");
    assert!(!state.text_input_commit_string("x"), "commit_string must no-op without an enabled field");

    // enable(1) + commit(7): the field becomes the active text input.
    c.conn.send(&Message::new(ti, 1)); // enable
    c.conn.send(&Message::new(ti, 7)); // commit
    pump!();
    assert!(state.text_input_active(), "a committed enable must make the field active");

    // Host → guest committed text: commit_string + done reach the client, with the committed serial.
    assert!(
        state.text_input_commit_string("héllo"),
        "commit_string must be delivered to the active field"
    );
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(
        c.last_commit_string.as_deref(),
        Some("héllo"),
        "the client should receive the committed UTF-8 text"
    );
    assert!(c.saw(ti, 5), "commit_string must be followed by done (opcode 5)");
    assert_eq!(
        c.last_done,
        Some(1),
        "done serial should equal the client's commit count (1)"
    );

    // Host → guest preedit (composing) text.
    assert!(state.text_input_preedit_string("ni", 0, 2), "preedit must reach the active field");
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(
        c.last_preedit,
        Some(("ni".to_string(), 0, 2)),
        "the client should receive the preedit string + cursor span"
    );

    // Host → guest delete-surrounding (IME replacing text around the cursor).
    assert!(state.text_input_delete_surrounding(1, 0), "delete_surrounding must reach the active field");
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(c.last_delete, Some((1, 0)), "the client should receive delete_surrounding_text(1,0)");

    // Content-type + surrounding-text state is accepted and applied on commit; the purpose is surfaced so
    // the host can pick an IME mode (2 == digits).
    c.conn.send(&Message::new(ti, 5).u32(0).u32(2)); // set_content_type(hint=none, purpose=digits)
    c.conn
        .send(&Message::new(ti, 3).string("abc").i32(3).i32(3)); // set_surrounding_text("abc", cursor, anchor)
    c.conn.send(&Message::new(ti, 6).i32(1).i32(2).i32(10).i32(12)); // set_cursor_rectangle
    c.conn.send(&Message::new(ti, 7)); // commit (serial → 2)
    pump!();
    assert_eq!(
        state.text_input_content_purpose(),
        Some(2),
        "the committed content purpose (digits=2) should be recorded"
    );

    // disable(2) + commit(7): the field stops being active; host text is no longer routed.
    c.conn.send(&Message::new(ti, 2)); // disable
    c.conn.send(&Message::new(ti, 7)); // commit
    pump!();
    assert!(!state.text_input_active(), "a committed disable must deactivate the field");
    assert!(!state.text_input_commit_string("y"), "commit_string must no-op once disabled");

    // Re-enable, then move keyboard focus to a SECOND toplevel: text-input focus follows the keyboard —
    // the client's text-input gets `leave` (old surface) then `enter` (new surface), and is deactivated
    // until it re-enables on the new focus.
    c.conn.send(&Message::new(ti, 1)); // enable
    c.conn.send(&Message::new(ti, 7)); // commit
    pump!();
    assert!(state.text_input_active(), "re-enable should re-activate the field");

    let enters_before = c.enter_count;
    let surface2 = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface2)); // create_surface
    let xdg2 = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(xdg2).u32(surface2)); // get_xdg_surface
    let toplevel2 = c.alloc();
    c.conn.send(&Message::new(xdg2, 1).u32(toplevel2)); // get_toplevel
    c.conn.send(&Message::new(surface2, 6)); // commit → maps → focus moves
    pump!();
    assert_eq!(c.leave_count, 1, "moving focus should deliver exactly one text-input leave");
    assert_eq!(
        c.enter_count,
        enters_before + 1,
        "moving focus should re-enter the text-input on the new surface"
    );
    assert!(
        !state.text_input_active(),
        "a focus move invalidates state: the field must re-enable before it is active again"
    );
    let _ = (toplevel, toplevel2); // silence unused (kept for clarity of the mapped objects)
}
