//! Robustness / stress proof: the compositor must be un-crashable by its clients.
//!
//! Everything here runs against ONE `Display` (per the wayland-server process-global-state note in
//! `client_roundtrip.rs`) but with SEVERAL clients, so a hostile/broken client can be shown NOT to take
//! down the well-behaved one. We assert the server survives:
//!   1. many simultaneous surfaces + rapid create/destroy churn (no leak/crash, frames keep flowing);
//!   2. a client dropped mid-handshake (surfaces created, socket closed before teardown) — its resources
//!      are reclaimed and the surviving client keeps working;
//!   3. a bogus request (a call on a non-existent object id) — the offending client is disconnected, the
//!      server and every other client survive;
//!   4. an oversized/malformed buffer (dimensions far exceeding its shm pool) — rejected without a crash.
//!
//! The persistent client `A` is verified to still commit→present after each abuse, which is the real
//! invariant: one bad guest never wedges the compositor or its neighbours.

use dd_compositor::{ClientState, DdState};
use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;

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

struct Cli {
    conn: Conn,
    next_id: u32,
    globals: HashMap<String, (u32, u32)>,
}
impl Cli {
    fn new(fd: RawFd) -> Cli {
        Cli {
            conn: Conn::new(fd),
            next_id: 2,
            globals: HashMap::new(),
        }
    }
    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    fn drain(&mut self) {
        loop {
            match self.conn.fill() {
                Ok(0) | Ok(-1) | Err(_) => break,
                _ => {}
            }
        }
        while let Some(m) = self.conn.next_message() {
            if m.opcode == 0 && m.object == 2 {
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            }
        }
    }
    fn bind(&mut self, iface: &str, ver: u32) -> u32 {
        let id = self.alloc();
        let name = self.globals[iface].0;
        self.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
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

/// Create a fresh XRGB shm buffer of `w`x`h`, attach it to `surface`, and commit — the ordinary
/// guest→host present. Returns the buffer id (so it can later be destroyed for the churn test).
fn commit_buffer(c: &mut Cli, shm: u32, surface: u32, w: i32, h: i32) -> u32 {
    let stride = w * 4;
    let size = (stride * h) as usize;
    let pixels = vec![0x40u8; size];
    let mfd = dd_display::keymap::anon_fd_with(&pixels).expect("anon shm fd");
    let pool = c.alloc();
    c.conn.send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // create_pool
    c.conn.queue_fd(mfd);
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
    c.conn.send(&Message::new(surface, 6)); // commit
    c.conn.flush().unwrap();
    unsafe { libc::close(mfd) };
    buffer
}

#[test]
fn compositor_survives_stress_disconnect_and_bogus_requests() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(CountingPresenter { frames: 0 }));

    // insert_client on the shared display; returns the client-side fd wrapped in a Cli.
    let connect = |display: &mut Display<DdState>| -> Cli {
        let (client_fd, server_fd) = socketpair_nonblocking();
        display
            .handle()
            .insert_client(
                unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
                Arc::new(ClientState::default()),
            )
            .unwrap();
        Cli::new(client_fd)
    };

    // Dispatch that TOLERATES a per-client protocol error (the whole point: the server survives it).
    macro_rules! dispatch {
        () => {{
            let _ = display.dispatch_clients(&mut state);
            let _ = display.flush_clients();
        }};
    }

    // ---- Client A: the well-behaved, persistent client. Full handshake + a mapped, presented toplevel.
    let mut a = connect(&mut display);
    let reg = a.alloc();
    a.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    a.conn.flush().unwrap();
    dispatch!();
    a.drain();
    let comp = a.bind("wl_compositor", 4);
    let shm = a.bind("wl_shm", 1);
    let wm = a.bind("xdg_wm_base", 1);
    a.conn.flush().unwrap();
    dispatch!();
    a.drain();

    let a_surface = a.alloc();
    a.conn.send(&Message::new(comp, 0).u32(a_surface)); // create_surface
    let a_xdg = a.alloc();
    a.conn.send(&Message::new(wm, 2).u32(a_xdg).u32(a_surface)); // get_xdg_surface
    let a_top = a.alloc();
    a.conn.send(&Message::new(a_xdg, 1).u32(a_top)); // get_toplevel
    a.conn.send(&Message::new(a_surface, 6)); // commit (map)
    a.conn.flush().unwrap();
    dispatch!();
    commit_buffer(&mut a, shm, a_surface, 8, 6);
    dispatch!();
    assert!(state.presenter.frame_count() >= 1, "client A's first frame must present");

    // ---- (1) Many simultaneous surfaces + rapid create/destroy churn on client A.
    const N: usize = 64;
    let mut churn: Vec<(u32, u32, u32, u32)> = Vec::new(); // (surface, xdg, toplevel, buffer)
    for _ in 0..N {
        let s = a.alloc();
        a.conn.send(&Message::new(comp, 0).u32(s)); // create_surface
        let xs = a.alloc();
        a.conn.send(&Message::new(wm, 2).u32(xs).u32(s)); // get_xdg_surface
        let tl = a.alloc();
        a.conn.send(&Message::new(xs, 1).u32(tl)); // get_toplevel
        a.conn.send(&Message::new(s, 6)); // commit (map)
        a.conn.flush().unwrap();
        dispatch!();
        let buf = commit_buffer(&mut a, shm, s, 4, 4);
        dispatch!();
        churn.push((s, xs, tl, buf));
    }
    let frames_after_spawn = state.presenter.frame_count();
    assert!(
        frames_after_spawn > 1,
        "spawning {N} surfaces should have presented frames"
    );

    // Tear them all down rapidly (buffer.destroy, toplevel.destroy, xdg_surface.destroy, surface.destroy).
    for (s, xs, tl, buf) in churn.drain(..) {
        a.conn.send(&Message::new(buf, 0)); // wl_buffer.destroy
        a.conn.send(&Message::new(tl, 0)); // xdg_toplevel.destroy
        a.conn.send(&Message::new(xs, 0)); // xdg_surface.destroy
        a.conn.send(&Message::new(s, 0)); // wl_surface.destroy
    }
    a.conn.flush().unwrap();
    dispatch!();
    // A survives the churn and can still present.
    let before = state.presenter.frame_count();
    commit_buffer(&mut a, shm, a_surface, 8, 6);
    dispatch!();
    assert!(
        state.presenter.frame_count() > before,
        "client A must still present after mass create/destroy"
    );

    // ---- (2) A client dropped MID-HANDSHAKE: create surfaces, then close the socket before teardown.
    {
        let mut b = connect(&mut display);
        let breg = b.alloc();
        b.conn.send(&Message::new(WL_DISPLAY, 1).u32(breg));
        b.conn.flush().unwrap();
        dispatch!();
        b.drain();
        let bcomp = b.bind("wl_compositor", 4);
        b.conn.flush().unwrap();
        dispatch!();
        b.drain();
        // Create a couple of surfaces but never destroy them — then drop `b`, closing the fd abruptly.
        let bs1 = b.alloc();
        b.conn.send(&Message::new(bcomp, 0).u32(bs1));
        let bs2 = b.alloc();
        b.conn.send(&Message::new(bcomp, 0).u32(bs2));
        b.conn.flush().unwrap();
        dispatch!();
        // `b` (and its Conn) drops here → client fd closed mid-handshake.
    }
    // The server must reclaim B's resources without panicking, and A must be unaffected.
    dispatch!();
    let before = state.presenter.frame_count();
    commit_buffer(&mut a, shm, a_surface, 8, 6);
    dispatch!();
    assert!(
        state.presenter.frame_count() > before,
        "client A must still present after another client dropped mid-handshake"
    );

    // ---- (3) A bogus request: a call on an object id that was never allocated. Smithay posts a protocol
    // error and disconnects the offending client; the server + client A survive.
    {
        let mut d = connect(&mut display);
        let dreg = d.alloc();
        d.conn.send(&Message::new(WL_DISPLAY, 1).u32(dreg));
        d.conn.flush().unwrap();
        dispatch!();
        d.drain();
        // Opcode 0 on a wild, never-created object id — an invalid object reference.
        d.conn.send(&Message::new(0x00FF_FF00, 0).u32(1234));
        d.conn.flush().unwrap();
        dispatch!(); // must NOT panic (tolerant dispatch); d gets killed by the protocol error.
    }
    dispatch!();
    let before = state.presenter.frame_count();
    commit_buffer(&mut a, shm, a_surface, 8, 6);
    dispatch!();
    assert!(
        state.presenter.frame_count() > before,
        "client A must still present after a bogus request from another client"
    );

    // ---- (4) An oversized/malformed buffer: dimensions far exceeding the tiny shm pool. Rejected without
    // a crash (Smithay validates the pool bounds; our repack additionally refuses to read past the mapping).
    {
        let mut e = connect(&mut display);
        let ereg = e.alloc();
        e.conn.send(&Message::new(WL_DISPLAY, 1).u32(ereg));
        e.conn.flush().unwrap();
        dispatch!();
        e.drain();
        let eshm = e.bind("wl_shm", 1);
        e.conn.flush().unwrap();
        dispatch!();
        e.drain();
        // A 16-byte pool, but a buffer claiming 4096x4096 — wildly out of bounds.
        let pixels = vec![0u8; 16];
        let mfd = dd_display::keymap::anon_fd_with(&pixels).expect("anon shm fd");
        let pool = e.alloc();
        e.conn.send(&Message::new(eshm, 0).u32(pool).u32(16));
        e.conn.queue_fd(mfd);
        let buffer = e.alloc();
        e.conn.send(
            &Message::new(pool, 0)
                .u32(buffer)
                .i32(0)
                .i32(4096)
                .i32(4096)
                .i32(4096 * 4)
                .u32(1),
        );
        e.conn.flush().unwrap();
        unsafe { libc::close(mfd) };
        dispatch!(); // must NOT crash/OOM; e is disconnected by the invalid buffer.
    }
    dispatch!();
    let before = state.presenter.frame_count();
    commit_buffer(&mut a, shm, a_surface, 8, 6);
    dispatch!();
    assert!(
        state.presenter.frame_count() > before,
        "client A must still present after an oversized-buffer client was rejected"
    );
    let _ = a_top;
}
