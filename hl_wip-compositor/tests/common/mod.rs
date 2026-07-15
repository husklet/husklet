//! Shared plumbing for the exact-pixel COMPOSITOR demo tests (`demo_*.rs`).
//!
//! Each demo is its own test binary (it mutates process-global `$XDG_RUNTIME_DIR` / `$WAYLAND_DISPLAY`)
//! that drives a REAL in-process `wayland-client` against the live Smithay adapter, captures composited
//! frames through the [`PngPresenter`], and asserts EXACT pixel content + placement. This module holds
//! the boilerplate every demo repeats: standing up the compositor on a private socket, building
//! `wl_shm` buffers with drawn content, sampling captured pixels, and writing human-viewable PNGs.
//!
//! Not a test binary itself — it lives under `tests/common/` so cargo treats it as a shared module a
//! `demo_*.rs` file pulls in with `mod common;`.

#![allow(dead_code)]

use std::ffi::OsString;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::present::write_png;
use hl_compositor::adapter::smithay::{
    self, input_channel, CapturedFrame, InputCommand, InputSender, PngPresenter,
};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_registry::WlRegistry,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

/// Where the human-viewable PNGs land.
pub const DEMO_DIR: &str = "/tmp/hl-demo";

/// A running headless compositor on a private discovery socket, plus the seams a demo drives it through.
pub struct Harness {
    pub runtime_dir: PathBuf,
    pub stop: Arc<AtomicBool>,
    /// The presenter's captured-frame log (compositor thread writes, test thread reads).
    pub captures: Arc<Mutex<Vec<CapturedFrame>>>,
    /// Host input seam: inject pointer/keyboard events that reach the focused client on the wire.
    pub input_tx: InputSender<InputCommand>,
    /// The bound `wayland-N` socket name (already published to `$WAYLAND_DISPLAY`).
    pub socket_name: OsString,
    handle: Option<JoinHandle<()>>,
}

impl Harness {
    /// Stand up the compositor in a background thread on a private `$XDG_RUNTIME_DIR`, publish
    /// `$WAYLAND_DISPLAY`, and wait for the discovery socket to appear. `tag` names the private dir and
    /// the presenter's PNG dump directory.
    pub fn start(tag: &str) -> Harness {
        let runtime_dir = std::env::temp_dir().join(format!("hl-demo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        // SAFETY: each demo owns its whole test binary/process (a single #[test]).
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        std::env::remove_var("WAYLAND_SOCKET");

        let stop = Arc::new(AtomicBool::new(false));
        let presenter = PngPresenter::with_png_dir(PathBuf::from(DEMO_DIR).join(tag));
        let captures = presenter.captures();
        let (name_tx, name_rx) = mpsc::channel::<OsString>();
        let (input_tx, input_rx) = input_channel();

        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            smithay::run_auto_with_input(presenter, stop_thread, input_rx, move |name| {
                let _ = name_tx.send(name);
            })
            .expect("compositor serve loop (run_auto_with_input)");
        });

        let socket_name = name_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("run_auto_with_input never reported a bound socket name");
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        let socket_path = runtime_dir.join(&socket_name);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket_path.exists() {
            assert!(Instant::now() < deadline, "discovery socket {socket_path:?} never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }

        Harness { runtime_dir, stop, captures, input_tx, socket_name, handle: Some(handle) }
    }

    /// Stop the serve loop, join its thread, and remove the private runtime dir.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// Build a `w`x`h` `wl_shm` `Argb8888` buffer from tight top-left **BGRA** bytes (memory order for a
/// little-endian ARGB word). Each buffer gets its own pool + unlinked backing file whose mapping stays
/// live for the test.
pub fn make_buffer<T>(
    shm: &WlShm,
    qh: &QueueHandle<T>,
    dir: &Path,
    tag: &str,
    w: i32,
    h: i32,
    bgra: &[u8],
) -> WlBuffer
where
    T: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, ()> + 'static,
{
    let stride = w * 4;
    let size = (stride * h) as usize;
    assert_eq!(bgra.len(), size, "pixel buffer size mismatch for {tag}");
    let path = dir.join(format!("client-{tag}.shm"));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("shm file");
    file.write_all(bgra).expect("write shm pixels");
    file.flush().unwrap();
    let _ = std::fs::remove_file(&path); // unlink; the fd + mapping stay valid
    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    std::mem::forget(file); // the pool keeps the mapping alive via the fd
    pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ())
}

/// Like [`make_buffer`] but tags the `wl_buffer` with an explicit `wl_shm` `format`, and takes the pixel
/// bytes in the buffer's OWN memory order (whatever channel layout `format` implies — the caller lays out
/// the bytes to match). Used by the shm-format-coverage demo to attach the same logical color under
/// argb/xrgb/abgr/xbgr and assert the composited RGBA is identical.
pub fn make_buffer_fmt<T>(
    shm: &WlShm,
    qh: &QueueHandle<T>,
    dir: &Path,
    tag: &str,
    w: i32,
    h: i32,
    format: wl_shm::Format,
    bytes: &[u8],
) -> WlBuffer
where
    T: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, ()> + 'static,
{
    let stride = w * 4;
    let size = (stride * h) as usize;
    assert_eq!(bytes.len(), size, "pixel buffer size mismatch for {tag}");
    let path = dir.join(format!("client-{tag}.shm"));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("shm file");
    file.write_all(bytes).expect("write shm pixels");
    file.flush().unwrap();
    let _ = std::fs::remove_file(&path); // unlink; the fd + mapping stay valid
    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    std::mem::forget(file); // the pool keeps the mapping alive via the fd
    pool.create_buffer(0, w, h, stride, format, qh, ())
}

/// A `w`x`h` tight BGRA canvas filled with a solid RGBA color (little-endian ARGB memory order).
pub fn solid(w: i32, h: i32, rgba: [u8; 4]) -> Vec<u8> {
    let [r, g, b, a] = rgba;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        px.extend_from_slice(&[b, g, r, a]);
    }
    px
}

/// Paint an axis-aligned rect of `rgba` into a tight BGRA canvas of width `w` (clipped to bounds).
pub fn fill_rect(buf: &mut [u8], w: i32, h: i32, rx: i32, ry: i32, rw: i32, rh: i32, rgba: [u8; 4]) {
    let [r, g, b, a] = rgba;
    for y in ry.max(0)..(ry + rh).min(h) {
        for x in rx.max(0)..(rx + rw).min(w) {
            let i = ((y * w + x) * 4) as usize;
            buf[i] = b;
            buf[i + 1] = g;
            buf[i + 2] = r;
            buf[i + 3] = a;
        }
    }
}

/// Read the RGBA of a tight top-left BGRA canvas at `(x, y)`.
pub fn sample_bgra(buf: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [buf[i + 2], buf[i + 1], buf[i], buf[i + 3]]
}

/// Poll `pred` against the capture log until it matches or `secs` elapse.
pub fn wait_for(
    captures: &Arc<Mutex<Vec<CapturedFrame>>>,
    secs: u64,
    pred: impl Fn(&CapturedFrame) -> bool,
) -> Option<CapturedFrame> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(f) = captures.lock().unwrap().iter().rev().find(|f| pred(f)).cloned() {
            return Some(f);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Pump the client `queue` (so it processes server events and sends its own requests) while polling the
/// capture log for a frame matching `pred`. Returns the newest match, or `None` after `secs`.
pub fn pump_until<T>(
    queue: &mut EventQueue<T>,
    app: &mut T,
    captures: &Arc<Mutex<Vec<CapturedFrame>>>,
    secs: u64,
    pred: impl Fn(&CapturedFrame) -> bool,
) -> Option<CapturedFrame> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let _ = queue.roundtrip(app);
        if let Some(f) = captures.lock().unwrap().iter().rev().find(|f| pred(f)).cloned() {
            return Some(f);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Pump the client `queue` while polling an arbitrary condition on `app`.
pub fn pump_while<T>(
    queue: &mut EventQueue<T>,
    app: &mut T,
    secs: u64,
    done: impl Fn(&T) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if done(app) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        let _ = queue.roundtrip(app);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Convenience pixel probe on a captured frame.
pub trait PixelIs {
    fn pixel_is(&self, x: i32, y: i32, rgba: [u8; 4]) -> bool;
}
impl PixelIs for CapturedFrame {
    fn pixel_is(&self, x: i32, y: i32, rgba: [u8; 4]) -> bool {
        self.pixel(x, y) == Some(rgba)
    }
}

/// Write a single captured layer as `<DEMO_DIR>/<name>.png` for human confirmation.
pub fn save_frame(name: &str, frame: &CapturedFrame) {
    let _ = std::fs::create_dir_all(DEMO_DIR);
    let path = PathBuf::from(DEMO_DIR).join(format!("{name}.png"));
    let _ = write_png(&path, frame.width, frame.height, &frame.rgba);
}

/// Blend an ordered set of captured layers (bottom -> top) onto a `cw`x`ch` opaque canvas at each
/// layer's root-space `(x, y)`, and write it as `<DEMO_DIR>/<name>.png`. A genuine COMPOSITED image
/// (the presenter captures layers, not a blended framebuffer), so this reconstructs what a viewer sees.
pub fn save_composited(name: &str, cw: i32, ch: i32, bg: [u8; 4], layers: &[(&CapturedFrame, i32, i32)]) {
    let mut canvas = solid(cw, ch, bg);
    for (frame, ox, oy) in layers {
        for y in 0..frame.height {
            for x in 0..frame.width {
                let (dx, dy) = (ox + x, oy + y);
                if dx < 0 || dy < 0 || dx >= cw || dy >= ch {
                    continue;
                }
                let Some([r, g, b, a]) = frame.pixel(x, y) else { continue };
                let di = ((dy * cw + dx) * 4) as usize;
                canvas[di] = b;
                canvas[di + 1] = g;
                canvas[di + 2] = r;
                canvas[di + 3] = a;
            }
        }
    }
    // canvas is BGRA; write_png wants RGBA — repack.
    let mut rgba = vec![0u8; canvas.len()];
    for p in 0..(cw * ch) as usize {
        rgba[p * 4] = canvas[p * 4 + 2];
        rgba[p * 4 + 1] = canvas[p * 4 + 1];
        rgba[p * 4 + 2] = canvas[p * 4];
        rgba[p * 4 + 3] = canvas[p * 4 + 3];
    }
    let _ = std::fs::create_dir_all(DEMO_DIR);
    let path = PathBuf::from(DEMO_DIR).join(format!("{name}.png"));
    let _ = write_png(&path, cw, ch, &rgba);
}

// =============================== well-behaved "neighbor" client ===============================
//
// The robustness demos (`demo_*` batch 5) prove the adapter SURVIVES a hostile client: after driving
// abuse, a NORMAL client must still connect, map a toplevel, and composite an EXACT solid frame. That
// well-behaved path is identical across those demos, so it lives here as one reusable client instead of
// being re-plumbed per binary. Each demo also keeps its own hostile-client Dispatch types; this
// `Neighbor` type is a DISTINCT type, so the two never collide inside one test binary.

/// A minimal, correct Wayland client: binds globals, maps a solid-color toplevel, and drives the
/// map + first-frame handshake to completion. Its own `Connection`/`EventQueue` are held so it stays
/// alive (and can be pumped) for as long as the caller keeps it.
pub struct Neighbor {
    pub conn: Connection,
    pub queue: EventQueue<NeighborApp>,
    pub app: NeighborApp,
    pub width: i32,
    pub height: i32,
    pub color: [u8; 4],
}

/// Dispatch state for a [`Neighbor`]. A distinct type from any demo's own client `App`, so both coexist
/// in one test binary.
pub struct NeighborApp {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

impl Neighbor {
    /// Connect a fresh well-behaved client on the shared socket, map a `w`x`h` solid-`color` toplevel,
    /// and block until it is mapped and has drawn its first frame. Panics (fails the test) if the
    /// compositor never completes the handshake — the exact symptom of an adapter that a prior hostile
    /// client wedged.
    pub fn map(dir: &Path, tag: &str, w: i32, h: i32, color: [u8; 4]) -> Neighbor {
        let conn = Connection::connect_to_env().expect("neighbor connect_to_env");
        let (globals, mut queue) = registry_queue_init::<NeighborApp>(&conn).expect("neighbor registry init");
        let qh = queue.handle();

        let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

        let buffer = make_buffer(&shm, &qh, dir, tag, w, h, &solid(w, h, color));
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(format!("neighbor-{tag}"));
        surface.commit();

        let mut app = NeighborApp { surface: surface.clone(), buffer, drawn: false, frame_done: false };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(app.drawn && app.frame_done) {
            assert!(Instant::now() < deadline, "neighbor {tag} never mapped (adapter wedged?)");
            queue.blocking_dispatch(&mut app).expect("neighbor dispatch map");
        }
        // Keep the shell objects alive for the client's lifetime.
        std::mem::forget(toplevel);
        std::mem::forget(xdg);
        Neighbor { conn, queue, app, width: w, height: h, color }
    }

    /// Pump this client and assert it composited an EXACT solid-`color` frame — proof the whole
    /// wl → scene → present path still serves a normal client after abuse. Returns the captured frame.
    pub fn assert_presents(&mut self, captures: &Arc<Mutex<Vec<CapturedFrame>>>) -> CapturedFrame {
        let (w, h, color) = (self.width, self.height, self.color);
        let frame = pump_until(&mut self.queue, &mut self.app, captures, 5, move |f| {
            f.width == w && f.height == h && f.pixel_is(1, 1, color)
        })
        .expect("neighbor frame never composited after abuse (adapter did not survive)");
        // Exact solid fill: center + all four corners are the neighbor's color, nothing smeared.
        for (x, y) in [(w / 2, h / 2), (0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(frame.pixel(x, y).unwrap(), color, "neighbor pixel ({x},{y}) is its solid color");
        }
        frame
    }

    /// Roundtrip the client's queue once (drain server events, flush requests).
    pub fn pump(&mut self) {
        let _ = self.queue.roundtrip(&mut self.app);
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for NeighborApp {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<XdgWmBase, ()> for NeighborApp {
    fn event(_: &mut Self, wm: &XdgWmBase, e: <XdgWmBase as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e { wm.pong(serial); }
    }
}
impl Dispatch<XdgSurface, ()> for NeighborApp {
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.drawn {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, i32::MAX, i32::MAX);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for NeighborApp {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e { app.frame_done = true; }
    }
}
macro_rules! neighbor_ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for NeighborApp {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
neighbor_ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel);
