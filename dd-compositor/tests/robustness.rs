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

use dd_compositor::{ClientState, DdState, RenderLimits};
use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::{Arc, Mutex};

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

#[derive(Default)]
struct LifecycleLog {
    presented: Vec<u32>,
    dropped: Vec<u32>,
}

struct LifecyclePresenter(Arc<Mutex<LifecycleLog>>);
impl Presenter for LifecyclePresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.0.lock().unwrap().presented.push(surf.sid);
        Ok(PresentOutcome::Delivered { serial: 1, timing: None })
    }
    fn drop_window(&mut self, sid: u32) {
        self.0.lock().unwrap().dropped.push(sid);
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

    fn release_count(&mut self, buffer: u32) -> usize {
        loop {
            match self.conn.fill() {
                Ok(0) | Ok(-1) | Err(_) => break,
                _ => {}
            }
        }
        let mut count = 0;
        while let Some(message) = self.conn.next_message() {
            if message.object == buffer && message.opcode == 0 {
                count += 1;
            }
        }
        count
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
fn compositor_surface_identity_is_client_owned_generational_and_teardown_is_exact_once() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let dh = display.handle();
    let log = Arc::new(Mutex::new(LifecycleLog::default()));
    let mut state = DdState::new_with_render_limits(
        dh.clone(),
        Box::new(LifecyclePresenter(log.clone())),
        RenderLimits { surfaces_per_client: 1, ..RenderLimits::default() },
    );

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
    macro_rules! dispatch {
        () => {{
            let _ = display.dispatch_clients(&mut state);
            let _ = display.flush_clients();
        }};
    }
    fn bind_core(c: &mut Cli, display: &mut Display<DdState>, state: &mut DdState) -> (u32, u32, u32) {
        let reg = c.alloc();
        c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
        c.conn.flush().unwrap();
        let _ = display.dispatch_clients(state);
        let _ = display.flush_clients();
        c.drain();
        let comp = c.bind("wl_compositor", 4);
        let shm = c.bind("wl_shm", 1);
        let wm = c.bind("xdg_wm_base", 1);
        c.conn.flush().unwrap();
        let _ = display.dispatch_clients(state);
        let _ = display.flush_clients();
        c.drain();
        (comp, shm, wm)
    }
    fn map(c: &mut Cli, comp: u32, wm: u32) -> (u32, u32, u32) {
        let surface = c.alloc();
        c.conn.send(&Message::new(comp, 0).u32(surface));
        let xdg = c.alloc();
        c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface));
        let top = c.alloc();
        c.conn.send(&Message::new(xdg, 1).u32(top));
        c.conn.send(&Message::new(surface, 6));
        c.conn.flush().unwrap();
        (surface, xdg, top)
    }

    let mut a = connect(&mut display);
    let (ac, ash, awm) = bind_core(&mut a, &mut display, &mut state);
    let (asurf, axdg, atop) = map(&mut a, ac, awm);
    dispatch!();
    let abuf = commit_buffer(&mut a, ash, asurf, 4, 4);
    dispatch!();
    assert_eq!(
        a.release_count(abuf),
        1,
        "shm buffer must release exactly once after its pixels are copied"
    );
    a.conn.send(&Message::new(asurf, 1).u32(abuf).i32(0).i32(0));
    a.conn.send(&Message::new(asurf, 6));
    a.conn.flush().unwrap();
    dispatch!();
    assert_eq!(
        a.release_count(abuf),
        1,
        "reattaching the same proxy creates one new use and one new release"
    );

    let mut b = connect(&mut display);
    let (bc, bsh, bwm) = bind_core(&mut b, &mut display, &mut state);
    let (bsurf, _bxdg, _btop) = map(&mut b, bc, bwm);
    assert_eq!(
        asurf, bsurf,
        "fixture must exercise equal client-local protocol ids"
    );
    dispatch!();
    let bbuf = commit_buffer(&mut b, bsh, bsurf, 4, 4);
    dispatch!();
    assert_eq!(b.release_count(bbuf), 1);

    let ids = log.lock().unwrap().presented.clone();
    assert!(ids.len() >= 2);
    let a_host = ids[0];
    let b_host = *ids
        .iter()
        .find(|&&sid| sid != a_host)
        .expect("equal protocol ids from different clients must not alias");
    assert_eq!(state.render_usage_totals(), (2, 0, 128));

    a.conn.send(&Message::new(atop, 0));
    a.conn.send(&Message::new(axdg, 0));
    a.conn.send(&Message::new(asurf, 0));
    a.conn.flush().unwrap();
    dispatch!();
    dispatch!();
    let dropped = log.lock().unwrap().dropped.clone();
    assert_eq!(
        dropped.iter().filter(|&&sid| sid == a_host).count(),
        1,
        "role and surface teardown must collapse to one presenter reclamation"
    );
    assert!(
        !dropped.contains(&b_host),
        "destroying one client must not reclaim another client's surface"
    );
    assert_eq!(state.render_usage_totals(), (1, 0, 64));

    // B exceeds its own surface quota. Only B is disconnected; its first surface is refunded and A's
    // already-completed teardown remains untouched.
    let excess = b.alloc();
    b.conn.send(&Message::new(bc, 0).u32(excess));
    b.conn.flush().unwrap();
    dispatch!();
    dispatch!();
    drop(b);
    dispatch!();
    dispatch!();
    let dropped = log.lock().unwrap().dropped.clone();
    assert_eq!(
        dropped.iter().filter(|&&sid| sid == b_host).count(),
        1,
        "disconnect teardown must reclaim the surviving surface exactly once"
    );
    assert_eq!(
        state.render_usage_totals(),
        (0, 0, 0),
        "disconnect must refund every surface and CPU-cache charge"
    );
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
