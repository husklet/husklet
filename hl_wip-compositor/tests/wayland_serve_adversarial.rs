//! Adversarial end-to-end coverage of the `adapter/smithay` serve loop (wl → scene → present → pace),
//! driven by REAL `wayland-client`s over a temporary Unix socket — the same rig as `wayland_e2e`, pushed
//! into the hostile sequences the neutral scene tests can't reach because they never exercise the calloop
//! serve loop, the concrete `wl_callback` objects, or client lifetime.
//!
//! The headline proof is the throttle-stall fix: a second frame committed within one refresh interval is
//! throttled by the vsync pacer, and — crucially — the client then goes IDLE (commits nothing more). The
//! serve loop's repaint timer must still ship that retained frame ~one refresh later AND release the
//! `wl_surface.frame` callback the client is blocked on. Before the fix nothing re-drove `present_root`,
//! so the frame never reached the `PngPresenter` and the callback never fired — a hang the client's
//! bounded-deadline waits would surface as a timeout.
//!
//! Every assertion targets real evidence: `PngPresenter` captures (actual composed pixels) and real
//! client-side `wl_callback` / `wl_buffer.release` events — never "it didn't panic".

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{self, CapturedFrame, PngPresenter};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_registry::WlRegistry,
    wl_shm::{self, WlShm}, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 32;
const H: i32 = 24;

// ============================ compositor lifecycle ============================

/// A running compositor on a temp socket, torn down on drop.
struct Server {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    captures: Arc<Mutex<Vec<CapturedFrame>>>,
}

impl Server {
    fn start(tag: &str) -> Server {
        let socket = std::env::temp_dir().join(format!("hl-wip-adv-{}-{}.sock", tag, std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let stop = Arc::new(AtomicBool::new(false));
        let presenter = PngPresenter::new();
        let captures = presenter.captures();

        let socket_thread = socket.clone();
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            smithay::run(&socket_thread, presenter, stop_thread).expect("serve loop");
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "compositor socket never appeared");
            std::thread::sleep(Duration::from_millis(5));
        }
        Server { socket, stop, handle: Some(handle), captures }
    }

    fn captures(&self) -> Vec<CapturedFrame> {
        self.captures.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

// ============================ client harness ============================

#[derive(Default)]
struct AppData {
    /// Number of `xdg_surface.configure` events acked (one per mapped surface, at least).
    configures: u32,
    /// Number of `wl_callback.done` events received — a released `wl_surface.frame` callback.
    callbacks_done: u32,
    /// Number of `wl_buffer.release` events received.
    released: u32,
}

/// A connected client: its own connection, queue, and bound globals.
struct Client {
    conn: Connection,
    queue: EventQueue<AppData>,
    qh: QueueHandle<AppData>,
    compositor: WlCompositor,
    shm: WlShm,
    wm_base: XdgWmBase,
    app: AppData,
    /// shm-backing files kept alive so their mappings stay valid.
    _files: Vec<std::fs::File>,
}

impl Client {
    fn connect(socket: &Path) -> Client {
        let stream = UnixStream::connect(socket).expect("connect to compositor socket");
        // Non-blocking so the deadline-guarded pump never blocks indefinitely on a socket read.
        stream.set_nonblocking(true).expect("nonblocking client socket");
        let conn = Connection::from_socket(stream).expect("wayland connection");
        let (globals, queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
        Client { conn, queue, qh, compositor, shm, wm_base, app: AppData::default(), _files: Vec::new() }
    }

    /// Build a `wl_buffer` filled with `(r, g, b, 255)` (opaque). `wl_shm` Argb8888 is little-endian, so
    /// memory order is `[B, G, R, A]`.
    fn buffer(&mut self, r: u8, g: u8, b: u8) -> WlBuffer {
        let stride = W * 4;
        let size = (stride * H) as usize;
        let mut pixels = Vec::with_capacity(size);
        for _ in 0..(W * H) {
            pixels.extend_from_slice(&[b, g, r, 0xFF]);
        }
        // A process-wide unique suffix: cargo runs tests as threads in ONE process, so a path keyed only
        // by pid would collide across parallel tests and let one client truncate another's shm mid-write.
        static SHM_SEQ: AtomicU64 = AtomicU64::new(0);
        let uniq = SHM_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hl-wip-adv-{}-{}.shm", std::process::id(), uniq));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("shm file");
        file.write_all(&pixels).expect("write shm");
        file.flush().unwrap();
        let _ = std::fs::remove_file(&path);
        let pool: WlShmPool = self.shm.create_pool(file.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, W, H, stride, wl_shm::Format::Argb8888, &self.qh, ());
        self._files.push(file);
        buffer
    }

    /// Create a mapped toplevel: create surface, roll the configure handshake, return the surface.
    fn toplevel(&mut self, title: &str) -> WlSurface {
        let surface = self.compositor.create_surface(&self.qh, ());
        let xdg_surface = self.wm_base.get_xdg_surface(&surface, &self.qh, ());
        let toplevel = xdg_surface.get_toplevel(&self.qh, ());
        toplevel.set_title(title.into());
        surface.commit(); // empty initial commit → compositor answers with the first configure
        let want = self.app.configures + 1;
        self.pump_until(Duration::from_secs(5), |a| a.configures >= want, "surface never configured");
        surface
    }

    /// Attach `buffer`, damage the whole surface, optionally request a frame callback, and commit.
    fn commit(&mut self, surface: &WlSurface, buffer: &WlBuffer, with_callback: bool) {
        surface.attach(Some(buffer), 0, 0);
        surface.damage(0, 0, W, H);
        if with_callback {
            let _cb: WlCallback = surface.frame(&self.qh, ());
        }
        surface.commit();
        let _ = self.conn.flush();
    }

    /// A damage-only / callback-only commit with NO buffer attached this cycle.
    fn commit_no_buffer(&mut self, surface: &WlSurface, with_callback: bool) {
        if with_callback {
            let _cb: WlCallback = surface.frame(&self.qh, ());
        }
        surface.commit();
        let _ = self.conn.flush();
    }

    /// Dispatch client events until `pred(app)` holds or the deadline passes. Uses a non-blocking
    /// read+dispatch (never `blocking_dispatch`) so a REGRESSION that withholds the awaited event fails at
    /// the deadline instead of hanging the whole suite forever.
    fn pump_until(&mut self, timeout: Duration, mut pred: impl FnMut(&AppData) -> bool, msg: &str) {
        let deadline = Instant::now() + timeout;
        while !pred(&self.app) {
            assert!(Instant::now() < deadline, "{msg}");
            let _ = self.conn.flush();
            // Drain anything already buffered, then try one non-blocking socket read for more.
            let _ = self.queue.dispatch_pending(&mut self.app);
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read(); // non-blocking socket ⇒ returns promptly when there's nothing to read
            }
            let _ = self.queue.dispatch_pending(&mut self.app);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Pump events for `dur`, ignoring the result (lets server-driven events land during "idle").
    fn pump_for(&mut self, dur: Duration) {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            let _ = self.conn.flush();
            let _ = self.queue.dispatch_pending(&mut self.app);
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.app);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Wait until the shared captures log satisfies `pred`, or fail after `timeout`.
fn wait_captures(server: &Server, timeout: Duration, mut pred: impl FnMut(&[CapturedFrame]) -> bool, msg: &str) -> Vec<CapturedFrame> {
    let deadline = Instant::now() + timeout;
    loop {
        let caps = server.captures();
        if pred(&caps) {
            return caps;
        }
        assert!(Instant::now() < deadline, "{msg} (captures so far: {})", server.captures().len());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn color_of(f: &CapturedFrame) -> [u8; 4] {
    f.pixel(W / 2, H / 2).expect("center pixel")
}

// ============================ tests ============================

/// THE fix: a frame throttled within one refresh interval, followed by client idle, must still ship one
/// refresh later AND release the frame callback the client requested on it. Without the serve loop's
/// repaint timer the retained frame never reaches the presenter and the callback never fires.
#[test]
fn throttled_commit_then_idle_ships_and_releases_the_frame_callback() {
    let server = Server::start("throttle");
    let mut client = Client::connect(&server.socket);
    let surface = client.toplevel("throttle");

    let red = client.buffer(0xF0, 0x10, 0x10);
    let blue = client.buffer(0x10, 0x10, 0xF0);

    // Frame 1 (red) is the first present → always due → ships immediately, releasing its callback.
    client.commit(&surface, &red, true);
    // Frame 2 (blue) committed immediately after → lands within one refresh interval → THROTTLED.
    client.commit(&surface, &blue, true);

    // The client now goes idle: it commits nothing more, only pumping to receive server events. The only
    // thing that can ship frame 2 + fire its callback is the serve loop's repaint timer.
    client.pump_until(
        Duration::from_secs(5),
        |a| a.callbacks_done >= 2,
        "the throttled frame's callback never released — repaint timer did not re-drive present",
    );

    // And the blue frame actually reached the presenter (stale content did not persist).
    let caps = wait_captures(
        &server,
        Duration::from_secs(5),
        |c| c.len() >= 2 && color_of(c.last().unwrap()) == [0x10, 0x10, 0xF0, 0xFF],
        "the throttled (blue) frame never shipped to the presenter",
    );
    assert_eq!(color_of(&caps[0]), [0xF0, 0x10, 0x10, 0xFF], "first shipped frame is red");
    assert_eq!(color_of(caps.last().unwrap()), [0x10, 0x10, 0xF0, 0xFF], "the retained blue frame shipped");
}

/// A burst of commits inside one refresh interval coalesces: the compositor presents the first, then ONE
/// re-driven frame for the whole burst — never one present per commit.
#[test]
fn a_burst_of_commits_coalesces_to_far_fewer_presents() {
    let server = Server::start("coalesce");
    let mut client = Client::connect(&server.socket);
    let surface = client.toplevel("coalesce");

    // First frame ships.
    let first = client.buffer(0x20, 0x20, 0x20);
    client.commit(&surface, &first, false);
    wait_captures(&server, Duration::from_secs(5), |c| c.len() >= 1, "first frame never shipped");

    // Ten more frames committed back-to-back (all well within one 16.6 ms interval), then idle.
    let bursts: Vec<WlBuffer> = (0..10).map(|i| client.buffer(0x30 + i * 4, 0x00, 0x00)).collect();
    for b in &bursts {
        client.commit(&surface, b, false);
    }
    // Let the repaint timer settle the coalesced frame.
    client.pump_for(Duration::from_millis(200));
    let caps = wait_captures(&server, Duration::from_secs(5), |c| c.len() >= 2, "the coalesced frame never shipped");

    // 1 (first) + 1 coalesced = 2 expected; allow a small tolerance for a burst that straddles a boundary,
    // but it must be FAR below the 11 total commits — proof the pacer coalesced instead of one-per-commit.
    assert!(
        caps.len() >= 2 && caps.len() <= 4,
        "expected the 10-commit burst to coalesce to ~1 extra present, got {} total presents",
        caps.len()
    );
}

/// A commit with no buffer attached (a bare frame-callback / damage-only commit) never captures a frame,
/// but its callback still releases (a clean tree is "skipped", which completes callbacks) — no stall.
#[test]
fn no_buffer_commit_captures_nothing_but_releases_its_callback() {
    let server = Server::start("nobuf");
    let mut client = Client::connect(&server.socket);
    let surface = client.toplevel("nobuf");

    // Commit with NO buffer, requesting a frame callback. Nothing to compose → nothing captured.
    client.commit_no_buffer(&surface, true);
    client.pump_until(
        Duration::from_secs(5),
        |a| a.callbacks_done >= 1,
        "a no-buffer commit's frame callback never released",
    );
    // Give any (erroneous) present a chance to land, then assert none did.
    client.pump_for(Duration::from_millis(100));
    assert!(server.captures().is_empty(), "a bufferless commit must not present a frame");

    // The compositor is still healthy: a real buffer now presents.
    let green = client.buffer(0x10, 0xE0, 0x10);
    client.commit(&surface, &green, false);
    wait_captures(&server, Duration::from_secs(5), |c| c.len() >= 1, "a real frame after a bufferless commit never shipped");
}

/// A client that commits a throttled frame and then disconnects mid-flight must not wedge or crash the
/// serve loop: a fresh client connects afterward and presents normally.
#[test]
fn client_disconnect_mid_throttle_does_not_wedge_the_compositor() {
    let server = Server::start("disc");
    {
        let mut client = Client::connect(&server.socket);
        let surface = client.toplevel("doomed");
        let a = client.buffer(0xC0, 0x00, 0x00);
        let b = client.buffer(0x00, 0x00, 0xC0);
        client.commit(&surface, &a, false); // ships (first present)
        client.commit(&surface, &b, true); // throttled + a pending frame callback + pending repaint
        let _ = client.conn.flush();
        // Drop the client immediately — connection closes with a repaint still armed for its surface.
    }

    // A brand-new client must still be served and present a frame.
    let mut client2 = Client::connect(&server.socket);
    let surface2 = client2.toplevel("survivor");
    let ok = client2.buffer(0x10, 0xB0, 0xE0);
    client2.commit(&surface2, &ok, true);
    client2.pump_until(Duration::from_secs(5), |a| a.callbacks_done >= 1, "second client never got its callback");
    wait_captures(
        &server,
        Duration::from_secs(5),
        |c| c.iter().any(|f| color_of(f) == [0x10, 0xB0, 0xE0, 0xFF]),
        "the surviving client's frame never shipped",
    );
}

/// Two interleaved toplevels pace independently: both distinct colors reach the presenter.
#[test]
fn two_interleaved_toplevels_both_present() {
    let server = Server::start("multi");
    let mut client = Client::connect(&server.socket);
    let s1 = client.toplevel("one");
    let s2 = client.toplevel("two");

    let c1 = client.buffer(0xE0, 0x30, 0x30); // reddish
    let c2 = client.buffer(0x30, 0x30, 0xE0); // bluish

    // Interleave commits across the two surfaces.
    client.commit(&s1, &c1, true);
    client.commit(&s2, &c2, true);
    client.commit(&s1, &c1, true);
    client.commit(&s2, &c2, true);

    // Both surfaces must have received their callbacks and shipped their colors.
    client.pump_until(Duration::from_secs(5), |a| a.callbacks_done >= 2, "both toplevels never released callbacks");
    wait_captures(
        &server,
        Duration::from_secs(5),
        |c| {
            c.iter().any(|f| color_of(f) == [0xE0, 0x30, 0x30, 0xFF])
                && c.iter().any(|f| color_of(f) == [0x30, 0x30, 0xE0, 0xFF])
        },
        "both interleaved toplevels never both shipped",
    );
}

// ============================ wayland-client Dispatch plumbing ============================

impl Dispatch<WlRegistry, GlobalListContents> for AppData {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<XdgWmBase, ()> for AppData {
    fn event(_: &mut Self, wm_base: &XdgWmBase, event: <XdgWmBase as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for AppData {
    fn event(app: &mut Self, xdg_surface: &XdgSurface, event: <XdgSurface as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            app.configures += 1;
        }
    }
}

impl Dispatch<WlBuffer, ()> for AppData {
    fn event(app: &mut Self, _: &WlBuffer, event: <WlBuffer as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_buffer::Event::Release = event {
            app.released += 1;
        }
    }
}

impl Dispatch<WlCallback, ()> for AppData {
    fn event(app: &mut Self, _: &WlCallback, event: <WlCallback as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            app.callbacks_done += 1;
        }
    }
}

macro_rules! ignore_dispatch {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for AppData {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_dispatch!(WlCompositor, WlSurface, WlShm, WlShmPool, XdgToplevel);
