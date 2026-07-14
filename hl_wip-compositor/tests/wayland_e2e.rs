//! Headless end-to-end proof of the `adapter/smithay` layer: wl → scene → present.
//!
//! Spins up the real compositor (`adapter::smithay::serve::run`) on a temporary Unix socket in a
//! background thread, connects a REAL `wayland-client`, creates a `wl_compositor` surface + `xdg_toplevel`,
//! attaches a `wl_shm` buffer filled with a known color, and commits. The compositor decodes the wire
//! through Smithay, drives the neutral `scene` policy (commit → compose → present), and the `PngPresenter`
//! captures the composed frame. The test then asserts the captured pixels match the color the client drew
//! — proving the whole path with no display, no GPU.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{self, PngPresenter};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
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
const R: u8 = 0x20;
const G: u8 = 0x80;
const B: u8 = 0xF0;
const A: u8 = 0xFF;

struct AppData {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
    released: bool,
}

fn main_test(png_dir: PathBuf) {
    // ---- 1. Start the compositor on a temp socket in a background thread --------------------------
    let socket = std::env::temp_dir().join(format!("hl-wip-e2e-{}.sock", std::process::id()));
    let stop = Arc::new(AtomicBool::new(false));

    let presenter = PngPresenter::with_png_dir(png_dir.clone());
    let captures = presenter.captures();

    let socket_thread = socket.clone();
    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        smithay::run(&socket_thread, presenter, stop_thread).expect("compositor serve loop");
    });

    // Wait for the socket to appear.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "compositor socket never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 2. Connect a real wayland-client ---------------------------------------------------------
    let stream = UnixStream::connect(&socket).expect("connect to compositor socket");
    let conn = Connection::from_socket(stream).expect("wayland connection");
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor global");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm global");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base global");

    // ---- 3. Build a shm buffer filled with the known color ----------------------------------------
    let stride = W * 4;
    let size = (stride * H) as usize;
    let mut pixels = Vec::with_capacity(size);
    for _ in 0..(W * H) {
        pixels.extend_from_slice(&[B, G, R, A]); // little-endian ARGB
    }
    let shm_path = std::env::temp_dir().join(format!("hl-wip-e2e-{}.shm", std::process::id()));
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

    // ---- 4. Create the toplevel and drive the configure/ack handshake -----------------------------
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("hl-wip-e2e".into());
    surface.commit(); // initial empty commit → compositor answers with the first configure

    let mut app = AppData { surface: surface.clone(), buffer: buffer.clone(), configured: false, released: false };

    // Pump the client queue until the surface is configured (the Configure handler attaches the buffer
    // and commits it) and the compositor has released the buffer (proof it consumed our pixels).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.configured && app.released) {
        assert!(Instant::now() < deadline, "configure/release handshake timed out");
        queue.blocking_dispatch(&mut app).expect("client dispatch");
    }

    // ---- 5. Wait for the presenter to capture the composed frame ----------------------------------
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(f) = captures.lock().unwrap().first().cloned() {
            break f;
        }
        assert!(Instant::now() < deadline, "presenter never captured a frame");
        std::thread::sleep(Duration::from_millis(10));
    };

    // ---- 6. Assert the client's pixels arrived intact ---------------------------------------------
    assert_eq!(frame.width, W, "captured width");
    assert_eq!(frame.height, H, "captured height");
    let center = frame.pixel(W / 2, H / 2).expect("center pixel present");
    assert_eq!(center, [R, G, B, A], "center pixel matches the color the client drew");
    // Every pixel is that color (the client filled the whole buffer).
    let corner = frame.pixel(0, 0).expect("corner pixel");
    assert_eq!(corner, [R, G, B, A], "corner pixel matches");

    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(png.exists(), "a real PNG was written to {png:?}");

    // ---- 7. Shut the compositor down --------------------------------------------------------------
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn wayland_client_buffer_reaches_png_presenter() {
    let dir = std::env::temp_dir().join(format!("hl-wip-e2e-png-{}", std::process::id()));
    main_test(dir);
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
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            // First configure: attach the colored buffer, damage the whole surface, and commit it.
            if !app.configured {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
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
