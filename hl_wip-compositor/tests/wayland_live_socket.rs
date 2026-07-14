//! Live-socket end-to-end proof: a REAL client discovers the compositor the way a real GUI toolkit does.
//!
//! Where `wayland_e2e` / `wayland_serve_adversarial` hand the client an already-opened socket stream
//! (`Connection::from_socket`), this test exercises the FULL real-world discovery path a weston /
//! GTK / Chrome client takes:
//!
//!   1. The compositor binds the STANDARD Wayland discovery socket via Smithay's `ListeningSocketSource`
//!      (`adapter::smithay::run_auto`) — `$XDG_RUNTIME_DIR/wayland-N` plus its sibling `.lock` file — not
//!      a bespoke absolute path the client is spoon-fed.
//!   2. The client connects with `Connection::connect_to_env()`, which reads `$WAYLAND_DISPLAY` and joins
//!      `$XDG_RUNTIME_DIR` — the exact libwayland `wl_display_connect(NULL)` behaviour.
//!   3. The client drives the real protocol sequence `wl_registry` → `wl_compositor` / `wl_shm` /
//!      `xdg_wm_base` → `wl_surface` → `xdg_surface` → `xdg_toplevel` → ack configure → attach a colored
//!      `wl_shm` buffer → damage → commit, requesting a `wl_surface.frame` callback.
//!   4. We assert the SERVER side completed (the client received its `xdg_surface.configure`, its
//!      `wl_buffer.release`, and its `wl_surface.frame` callback) AND that the committed pixels composited
//!      all the way to the `PngPresenter` at the expected coordinates — real socket, real wire, real
//!      composite, fully headless (no DRM, no display, no GPU).
//!
//! This is a single `#[test]` in its own test binary on purpose: it mutates `$XDG_RUNTIME_DIR` /
//! `$WAYLAND_DISPLAY` (process-global), and a dedicated binary is its own process, so the mutation cannot
//! race any other test.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{self, PngPresenter};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::{self, WlShm}, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 64;
const H: i32 = 48;
// The color the client paints. `wl_shm` Argb8888 is 32-bit little-endian → memory bytes `[B, G, R, A]`.
const R: u8 = 0x11;
const G: u8 = 0xAA;
const B: u8 = 0x77;
const A: u8 = 0xFF;

struct AppData {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
    released: bool,
    frame_done: bool,
}

#[test]
fn real_client_discovers_compositor_via_wayland_display_and_composites() {
    // ---- 1. A private XDG_RUNTIME_DIR so the discovery socket lands in an isolated, 0700 dir ----------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-live-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    // Make sure no inherited WAYLAND_SOCKET fd short-circuits `connect_to_env` before it reads DISPLAY.
    std::env::remove_var("WAYLAND_SOCKET");

    let png_dir = runtime_dir.join("png");

    // ---- 2. Start the compositor on the STANDARD discovery socket in a background thread --------------
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(png_dir.clone());
    let captures = presenter.captures();
    let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();

    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        smithay::run_auto(presenter, stop_thread, move |name| {
            let _ = name_tx.send(name);
        })
        .expect("compositor serve loop (run_auto)");
    });

    // The socket name Smithay chose (`wayland-N`) — publish it as $WAYLAND_DISPLAY for discovery.
    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto never reported a bound socket name");
    let name_str = socket_name.to_string_lossy();
    assert!(
        name_str.starts_with("wayland-"),
        "expected a standard `wayland-N` discovery name, got {name_str:?}",
    );
    let socket_path = runtime_dir.join(&socket_name);
    let lock_path = runtime_dir.join(format!("{name_str}.lock"));
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    // Wait for the discovery socket (and its lock file) to actually exist on disk.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(Instant::now() < deadline, "discovery socket {socket_path:?} never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(lock_path.exists(), "the standard `.lock` file was not created next to the socket");

    // ---- 3. Connect a REAL client the standard way: $WAYLAND_DISPLAY discovery ------------------------
    let conn = Connection::connect_to_env()
        .expect("connect_to_env failed — client could not discover the compositor via $WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor global");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm global");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base global");

    // ---- 4. Build a wl_shm buffer filled with the known color ----------------------------------------
    let stride = W * 4;
    let size = (stride * H) as usize;
    let mut pixels = Vec::with_capacity(size);
    for _ in 0..(W * H) {
        pixels.extend_from_slice(&[B, G, R, A]); // little-endian ARGB
    }
    let shm_path = runtime_dir.join("client.shm");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&shm_path)
        .expect("shm file");
    file.write_all(&pixels).expect("write shm pixels");
    file.flush().unwrap();
    let _ = std::fs::remove_file(&shm_path); // unlink; the fd (and its mapping) stays valid

    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let buffer: WlBuffer = pool.create_buffer(0, W, H, stride, wl_shm::Format::Argb8888, &qh, ());

    // ---- 5. Create the toplevel and drive the configure/ack handshake --------------------------------
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("hl-wip-live".into());
    surface.commit(); // initial empty commit → compositor answers with the first configure

    let mut app = AppData {
        surface: surface.clone(),
        buffer: buffer.clone(),
        configured: false,
        released: false,
        frame_done: false,
    };

    // The server side is complete when: it configured our surface, released our buffer (proof it consumed
    // the pixels), and fired the frame callback we requested on the commit (proof the frame presented).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.configured && app.released && app.frame_done) {
        assert!(
            Instant::now() < deadline,
            "server-side handshake incomplete: configured={} released={} frame_done={}",
            app.configured, app.released, app.frame_done,
        );
        queue.blocking_dispatch(&mut app).expect("client dispatch");
    }

    // ---- 6. Assert the committed pixels composited all the way to the presenter ----------------------
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(f) = captures.lock().unwrap().first().cloned() {
            break f;
        }
        assert!(Instant::now() < deadline, "presenter never captured a frame");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(frame.width, W, "captured width");
    assert_eq!(frame.height, H, "captured height");
    assert_eq!(
        frame.pixel(W / 2, H / 2).expect("center pixel"),
        [R, G, B, A],
        "center pixel matches the color the real client drew over the live socket",
    );
    assert_eq!(frame.pixel(0, 0).expect("corner pixel"), [R, G, B, A], "corner pixel matches");
    assert!(
        png_dir.join(format!("frame-{}.png", frame.serial)).exists(),
        "a real PNG of the composited frame was written",
    );

    // ---- 7. Shut the compositor down and clean up ----------------------------------------------------
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

// ------------------------- wayland-client Dispatch plumbing (client side) -------------------------

impl Dispatch<WlRegistry, GlobalListContents> for AppData {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for AppData {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for AppData {
    fn event(
        app: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            // First configure: attach the colored buffer, damage the whole surface, request a frame
            // callback, and commit.
            if !app.configured {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.configured = true;
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlBuffer,
        event: <WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_buffer::Event::Release = event {
            app.released = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        event: <WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            app.frame_done = true;
        }
    }
}

// The remaining objects emit events we don't need to act on.
macro_rules! ignore_dispatch {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for AppData {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {}
        }
    )*};
}
ignore_dispatch!(WlCompositor, WlSurface, WlShm, WlShmPool, XdgToplevel);
