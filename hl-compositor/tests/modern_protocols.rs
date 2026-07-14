//! Client-roundtrip proof for the modern GUI protocol groups composed from the vendored Smithay tree
//! (codex-rendering §5.2 / §9.4, ledger row `modern_gui_protocol_groups_are_composed_from_vendored_smithay`).
//!
//! A minimal in-process Wayland client (built on `hl_display::wire`) connects over a `socketpair`,
//! maps a toplevel to take keyboard+pointer focus, then binds and drives EACH newly composed protocol,
//! asserting the global is advertised, the client can bind it and create its objects, and a basic
//! request/event (or request → recorded host policy) exchange completes:
//!
//!   1. `zwp_pointer_gestures_v1`         — bind, create a swipe gesture, inject a swipe → begin+end events
//!   2. `zwp_tablet_manager_v2`           — bind, get_tablet_seat, hot-plug a virtual tablet → tablet_added
//!   3. `zwp_idle_inhibit_manager_v1`     — bind, create_inhibitor → host records intent; destroy → cleared
//!   4. `wp_content_type_manager_v1`      — bind, set_content_type(video) + commit → host stores the hint
//!   5. `zxdg_exporter_v2`/`zxdg_importer_v2` — export → real handle event; import that handle (no error)
//!   6. `zwp_keyboard_shortcuts_inhibit_manager_v1` — bind, inhibit_shortcuts → host activates → active event
//!
//! ONE `Display`/client per the note in `client_roundtrip.rs` about wayland-server's process-global
//! state. Runs headlessly on Linux (libxkbcommon present) and macOS.

use hl_compositor::{ClientState, HlState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::{Arc, Mutex};

const WL_DISPLAY: u32 = 1;

struct CountingPresenter {
    frames: u32,
    /// The HOST surface id of the last presented frame (the compositor keys per-surface state by this
    /// monotonic id, not by the client's protocol object id), so the test can address state accessors.
    last_sid: Arc<Mutex<Option<u32>>>,
}
impl Presenter for CountingPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.frames += 1;
        *self.last_sid.lock().unwrap() = Some(surf.sid);
        Ok(PresentOutcome::Delivered { serial: self.frames as u64, timing: None })
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
}

struct Cli {
    conn: Conn,
    next_id: u32,
    globals: HashMap<String, (u32, u32)>,
    /// Object id whose string first-arg events we want to capture (xdg_exported handle).
    handle_watch: u32,
    /// Captured `zxdg_exported_v2.handle(handle)` string.
    exported_handle: Option<String>,
    events: Vec<(u32, u16)>,
}

impl Cli {
    fn new(fd: RawFd) -> Cli {
        Cli {
            conn: Conn::new(fd),
            next_id: 2,
            globals: HashMap::new(),
            handle_watch: 0,
            exported_handle: None,
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
            } else if self.handle_watch != 0 && m.object == self.handle_watch && m.opcode == 0 {
                // zxdg_exported_v2.handle(handle) — the server-minted export handle string.
                self.exported_handle = Some(m.reader().string());
            }
        }
    }
    fn saw(&self, object: u32, opcode: u16) -> bool {
        self.events.contains(&(object, opcode))
    }
    /// True if a `wl_display.error` (object 1, opcode 0) was ever delivered — a protocol violation.
    fn had_protocol_error(&self) -> bool {
        self.saw(WL_DISPLAY, 0)
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
fn modern_protocols_bind_and_roundtrip() {
    let mut display: Display<HlState> = Display::new().unwrap();
    let mut dh = display.handle();
    let last_sid = Arc::new(Mutex::new(None));
    let mut state = HlState::new(dh.clone(), Box::new(CountingPresenter { frames: 0, last_sid: last_sid.clone() }));

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

    // Registry: every newly composed manager global must be advertised.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();
    for iface in [
        "zwp_pointer_gestures_v1",
        "zwp_tablet_manager_v2",
        "zwp_idle_inhibit_manager_v1",
        "wp_content_type_manager_v1",
        "zxdg_exporter_v2",
        "zxdg_importer_v2",
        "zwp_keyboard_shortcuts_inhibit_manager_v1",
    ] {
        assert!(
            c.globals.contains_key(iface),
            "global {iface} not advertised; got {:?}",
            c.globals.keys().collect::<Vec<_>>()
        );
    }

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

    // wl_seat.get_pointer(0): the pointer that gesture objects attach to.
    let pointer = c.alloc();
    c.conn.send(&Message::new(seat, 0).u32(pointer));

    // Map a toplevel → keyboard focus (and, after pointer_motion below, pointer focus).
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    let xdg = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    let toplevel = c.alloc();
    c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.conn.send(&Message::new(surface, 6)); // commit → maps → focus
    pump!();

    // Give the toplevel committed content (an 8×8 shm buffer). A pointer can only take focus over a
    // surface with real bounds, so without this the gesture below has no focused surface to reach.
    let shm = bind(&mut c, "wl_shm", 1);
    let (bw, bh) = (8i32, 8i32);
    let stride = bw * 4;
    let bsize = (stride * bh) as usize;
    let bfd = hl_display::keymap::anon_fd_with(&vec![0u8; bsize]).expect("anon shm fd");
    let pool = c.alloc();
    c.conn.send(&Message::new(shm, 0).u32(pool).u32(bsize as u32)); // create_pool
    c.conn.queue_fd(bfd);
    let buffer = c.alloc();
    c.conn.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(bw).i32(bh).i32(stride).u32(1)); // create_buffer
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(surface, 6)); // commit → content
    pump!();
    unsafe { libc::close(bfd) };
    // The compositor keys per-surface state by a monotonic HOST id (not the client's protocol object id);
    // capture the toplevel's host id from its present so the content-type accessor below addresses it.
    let surface_sid = last_sid.lock().unwrap().expect("the toplevel's frame reached the presenter");

    // ---------------------------------------------------------------------------------------------
    // (1) zwp_pointer_gestures_v1: bind, create a swipe gesture for our pointer, give the pointer focus,
    // then inject a host swipe (the seam a macOS trackpad bridge would drive) → the client's swipe
    // gesture object must receive begin (opcode 0) and end (opcode 2).
    let gestures = bind(&mut c, "zwp_pointer_gestures_v1", 1);
    let swipe = c.alloc();
    c.conn.send(&Message::new(gestures, 0).u32(swipe).u32(pointer)); // get_swipe_gesture(id, pointer)
    pump!();
    state.pointer_motion(5.0, 5.0); // focus the pointer on the mapped toplevel
    state.inject_swipe_gesture(3);
    display.flush_clients().unwrap();
    c.drain();
    assert!(
        c.saw(swipe, 0),
        "expected zwp_pointer_gesture_swipe_v1.begin (opcode 0); saw {:?}",
        c.events
    );
    assert!(
        c.saw(swipe, 2),
        "expected zwp_pointer_gesture_swipe_v1.end (opcode 2); saw {:?}",
        c.events
    );

    // ---------------------------------------------------------------------------------------------
    // (2) zwp_tablet_manager_v2: bind, get_tablet_seat for our seat, then hot-plug a virtual tablet (the
    // mechanism a real digitizer would use) → the client's tablet_seat must receive tablet_added (0).
    // dd advertises ZERO tablets by default (no hardware); this proves the delegate delivers when one
    // appears.
    let tablet_mgr = bind(&mut c, "zwp_tablet_manager_v2", 1);
    let tablet_seat = c.alloc();
    c.conn
        .send(&Message::new(tablet_mgr, 0).u32(tablet_seat).u32(seat)); // get_tablet_seat(id, seat)
    pump!();
    state.add_tablet("hl-virtual-tablet");
    display.flush_clients().unwrap();
    c.drain();
    assert!(
        c.saw(tablet_seat, 0),
        "expected zwp_tablet_seat_v2.tablet_added (opcode 0) after add_tablet; saw {:?}",
        c.events
    );

    // ---------------------------------------------------------------------------------------------
    // (3) zwp_idle_inhibit_manager_v1: bind, create an inhibitor for the surface → the host records the
    // intent (idle_inhibited() flips true); destroying it clears the intent.
    assert!(!state.idle_inhibited(), "no idle inhibitor should exist yet");
    let idle_mgr = bind(&mut c, "zwp_idle_inhibit_manager_v1", 1);
    let inhibitor = c.alloc();
    c.conn
        .send(&Message::new(idle_mgr, 1).u32(inhibitor).u32(surface)); // create_inhibitor(id, surface)
    pump!();
    assert!(
        state.idle_inhibited(),
        "creating an idle inhibitor must record the keep-awake intent"
    );
    c.conn.send(&Message::new(inhibitor, 0)); // zwp_idle_inhibitor_v1.destroy
    pump!();
    assert!(
        !state.idle_inhibited(),
        "destroying the inhibitor must clear the recorded intent"
    );

    // ---------------------------------------------------------------------------------------------
    // (4) wp_content_type_manager_v1: bind, attach a content-type object to the surface, set it to
    // `video` (2), commit → the host stores the committed hint per surface.
    assert_eq!(state.content_type(surface_sid), None, "no content type set yet");
    let ct_mgr = bind(&mut c, "wp_content_type_manager_v1", 1);
    let ct = c.alloc();
    c.conn.send(&Message::new(ct_mgr, 1).u32(ct).u32(surface)); // get_surface_content_type(id, surface)
    c.conn.send(&Message::new(ct, 1).u32(2)); // set_content_type(video = 2)
    c.conn.send(&Message::new(surface, 6)); // commit → applies the double-buffered content type
    pump!();
    assert_eq!(
        state.content_type(surface_sid),
        Some(2),
        "the committed wp_content_type (video=2) should be stored by the host"
    );

    // ---------------------------------------------------------------------------------------------
    // (5) zxdg_exporter_v2 / zxdg_importer_v2: export the toplevel → the server mints a real handle
    // (zxdg_exported_v2.handle event); a second client object imports that handle without error — the
    // cross-client parenting round trip.
    let exporter = bind(&mut c, "zxdg_exporter_v2", 1);
    let exported = c.alloc();
    c.handle_watch = exported;
    c.conn
        .send(&Message::new(exporter, 1).u32(exported).u32(surface)); // export_toplevel(id, surface)
    pump!();
    let handle = c
        .exported_handle
        .clone()
        .expect("export_toplevel must reply with a real zxdg_exported_v2.handle string");
    assert!(!handle.is_empty(), "the exported handle must be non-empty");
    let importer = bind(&mut c, "zxdg_importer_v2", 1);
    let imported = c.alloc();
    c.conn
        .send(&Message::new(importer, 1).u32(imported).string(&handle)); // import_toplevel(id, handle)
    pump!();

    // ---------------------------------------------------------------------------------------------
    // (6) zwp_keyboard_shortcuts_inhibit_manager_v1: bind, request inhibition for (surface, seat) → the
    // host honours it (dd owns no conflicting chords) and immediately activates the inhibitor, so the
    // client receives `active` (opcode 0).
    let ksi_mgr = bind(&mut c, "zwp_keyboard_shortcuts_inhibit_manager_v1", 1);
    let ksi = c.alloc();
    c.conn
        .send(&Message::new(ksi_mgr, 1).u32(ksi).u32(surface).u32(seat)); // inhibit_shortcuts(id, surface, seat)
    pump!();
    assert!(
        c.saw(ksi, 0),
        "an honoured shortcuts inhibitor must send active (opcode 0); saw {:?}",
        c.events
    );

    // No protocol violation was raised anywhere in the exchange above.
    assert!(
        !c.had_protocol_error(),
        "a wl_display.error was delivered during the modern-protocol roundtrip; events {:?}",
        c.events
    );
    let _ = toplevel;
}
