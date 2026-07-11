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
use dd_display::present::PngPresenter;
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::os::unix::io::RawFd;
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;

struct Client {
    conn: Conn,
    next_id: u32,
    globals: std::collections::HashMap<String, (u32, u32)>,
}

impl Client {
    fn new(fd: RawFd) -> Client {
        Client {
            conn: Conn::new(fd),
            next_id: 2,
            globals: Default::default(),
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
            // wl_registry.global(name, iface, version): registry is id 2 in this tiny client.
            if m.opcode == 0 && m.object == 2 {
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
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
fn client_connects_globals_advertise_and_a_frame_presents() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let dir = std::env::temp_dir().join("dd-compositor-roundtrip");
    let mut state = DdState::new(dh.clone(), Box::new(PngPresenter::new(&dir)));

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
        }};
    }

    // get_registry → the compositor advertises all globals.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();
    c.drain();

    for iface in [
        "wl_compositor",
        "wl_subcompositor",
        "wl_shm",
        "xdg_wm_base",
        "wl_seat",
        "wl_output",
        "wp_viewporter",
        "wp_presentation",
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

    // surface + xdg toplevel + initial commit → configure.
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    let xdg = c.alloc();
    c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    let toplevel = c.alloc();
    c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.conn.send(&Message::new(surface, 6)); // commit (no buffer)
    pump!();
    c.conn.send(&Message::new(xdg, 4).u32(1)); // ack_configure(serial=1)

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
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(surface, 2).i32(0).i32(0).i32(w).i32(h)); // damage
    c.conn.send(&Message::new(surface, 6)); // commit → present
    pump!();

    assert!(
        state.presenter.frame_count() >= 1,
        "the committed frame did not reach the presenter"
    );
}

use std::os::unix::io::FromRawFd;
