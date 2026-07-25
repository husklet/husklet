//! Live-socket proof of the xdg-shell configure + xdg-decoration negotiation a real desktop toolkit
//! (GTK4 / Chromium-ozone / Qt) requires to map, size, and decorate a window.
//!
//! Where `wayland_live_socket` proves a *bare* toplevel maps and composites (the weston-simple-egl
//! path), the bigger toolkits exercise more of xdg-shell before they will draw a single frame:
//!
//!   * they read the `xdg_toplevel.configure` for a **size**, its **states** array (is the window
//!     `Activated`? `Maximized`?), and the v4+ `configure_bounds` (the largest they should size to), and
//!   * they bind `zxdg_decoration_manager_v1`, create a `zxdg_toplevel_decoration_v1`, request a mode,
//!     and BLOCK until the compositor answers with a `zxdg_toplevel_decoration_v1.configure(mode)` that
//!     tells them whether to draw their own client-side decorations. A compositor that advertises no
//!     decoration manager — or that never answers `set_mode` — leaves such a client hung or
//!     double-decorated.
//!
//! This test drives that exact sequence over the STANDARD discovery socket (`run_auto` +
//! `Connection::connect_to_env`) and asserts the server side answered all of it, then maps a real buffer
//! and asserts it composited to the `PngPresenter`. Its own test binary because it mutates process-global
//! `$XDG_RUNTIME_DIR` / `$WAYLAND_DISPLAY`.

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
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_registry::WlRegistry,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, Mode, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 64;
const H: i32 = 48;
// The color the client paints. `wl_shm` Argb8888 is 32-bit little-endian → memory bytes `[B, G, R, A]`.
const R: u8 = 0x22;
const G: u8 = 0x55;
const B: u8 = 0xEE;
const A: u8 = 0xFF;

/// The `xdg_toplevel.state` enum discriminant for `activated` (xdg-shell v1: maximized=1, fullscreen=2,
/// resizing=3, activated=4, …). The states array is a packed list of these 32-bit LE values.
const STATE_ACTIVATED: u32 = 4;

/// Decode an `xdg_toplevel.configure` states array (packed 32-bit LE enum values) into a Vec of u32.
fn decode_states(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

struct AppData {
    surface: WlSurface,
    buffer: WlBuffer,
    mapped: bool,
    released: bool,
    frame_done: bool,
    // ---- xdg_toplevel.configure fidelity a real toolkit reads before mapping ----
    /// Latest `xdg_toplevel.configure` size `(w, h)`.
    toplevel_size: Option<(i32, i32)>,
    /// Latest `xdg_toplevel.configure` states array (decoded enum values).
    toplevel_states: Vec<u32>,
    /// Latest v4+ `xdg_toplevel.configure_bounds` `(w, h)` — the max the client should size to.
    toplevel_bounds: Option<(i32, i32)>,
    /// Whether ANY `xdg_surface.configure` (with a serial to ack) has been seen.
    xdg_surface_configured: bool,
    // ---- zxdg_toplevel_decoration_v1 negotiation ----
    /// Latest decoration mode the compositor configured (the CSD-vs-SSD answer the toolkit waits for).
    decoration_mode: Option<Mode>,
}

#[test]
fn real_client_negotiates_xdg_shell_configure_and_decoration_then_composites() {
    // ---- 1. Private XDG_RUNTIME_DIR so the discovery socket lands in an isolated 0700 dir -------------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-deco-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
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

    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto never reported a bound socket name");
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    let socket_path = runtime_dir.join(&socket_name);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "discovery socket {socket_path:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 3. Connect the standard way and bind the toolkit globals ------------------------------------
    let conn = Connection::connect_to_env().expect(
        "connect_to_env failed — client could not discover the compositor via $WAYLAND_DISPLAY",
    );
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
    let qh = queue.handle();

    // The compositor MUST advertise exactly one decoration manager — this is the global whose absence
    // makes GTK/Chrome fall back to (or hang on) decoration negotiation.
    let global_list = globals.contents().clone_list();
    let deco_count = global_list
        .iter()
        .filter(|g| g.interface == "zxdg_decoration_manager_v1")
        .count();
    assert_eq!(
        deco_count, 1,
        "expected exactly one zxdg_decoration_manager_v1 global, got {global_list:?}",
    );

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor global");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm global");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base global");
    let deco_mgr: ZxdgDecorationManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("zxdg_decoration_manager_v1 global");

    // ---- 4. A colored wl_shm buffer to map once configured -------------------------------------------
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
    let _ = std::fs::remove_file(&shm_path);
    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let buffer: WlBuffer = pool.create_buffer(0, W, H, stride, wl_shm::Format::Argb8888, &qh, ());

    // ---- 5. Drive the full toolkit sequence: surface → xdg_surface → toplevel → title/app_id →
    //         decoration → set_mode(ClientSide) → initial commit -------------------------------------
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("hl-wip-deco".into());
    toplevel.set_app_id("org.hl.wip.deco".into());

    // Create the decoration object and REQUEST client-side. A compositor that merely defaulted to
    // server-side would answer ServerSide; honoring the client's preference proves the `set_mode` path.
    let decoration: ZxdgToplevelDecorationV1 = deco_mgr.get_toplevel_decoration(&toplevel, &qh, ());
    decoration.set_mode(Mode::ClientSide);

    surface.commit(); // initial empty commit → compositor answers with configure(s)

    let mut app = AppData {
        surface: surface.clone(),
        buffer: buffer.clone(),
        mapped: false,
        released: false,
        frame_done: false,
        toplevel_size: None,
        toplevel_states: Vec::new(),
        toplevel_bounds: None,
        xdg_surface_configured: false,
        decoration_mode: None,
    };

    // The negotiation is complete when the client has received: a toplevel configure carrying a size, a
    // configure_bounds, a decoration configure carrying a mode, and (proof the mapped buffer presented)
    // its buffer release + frame callback.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.toplevel_size.is_some()
        && app.toplevel_bounds.is_some()
        && app.decoration_mode.is_some()
        && app.released
        && app.frame_done)
    {
        assert!(
            Instant::now() < deadline,
            "xdg/decoration handshake incomplete: size={:?} bounds={:?} mode={:?} released={} frame_done={}",
            app.toplevel_size, app.toplevel_bounds, app.decoration_mode, app.released, app.frame_done,
        );
        queue.blocking_dispatch(&mut app).expect("client dispatch");
    }

    // ---- 6. Assert the xdg_toplevel.configure fidelity a toolkit sizes/decorates against -------------
    let (tw, th) = app.toplevel_size.expect("toplevel configure size");
    assert!(
        tw > 0 && th > 0,
        "xdg_toplevel.configure must carry a non-zero size, got {tw}x{th}"
    );
    assert!(
        app.toplevel_states.contains(&STATE_ACTIVATED),
        "the mapped (focused) toplevel must be Activated, got states {:?}",
        app.toplevel_states,
    );
    // configure_bounds must match the scene's primary output logical size (HL-0 1920x1080 @ scale 1).
    assert_eq!(
        app.toplevel_bounds,
        Some((1920, 1080)),
        "configure_bounds must be the output logical size the client should cap itself to",
    );

    // ---- 7. Assert the decoration negotiation resolved to the mode the client asked for --------------
    assert_eq!(
        app.decoration_mode,
        Some(Mode::ClientSide),
        "compositor must honor the client's set_mode(ClientSide) so the toolkit knows to draw its own CSD",
    );

    // ---- 8. Assert the committed pixels composited all the way to the presenter ----------------------
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(f) = captures.lock().unwrap().first().cloned() {
            break f;
        }
        assert!(
            Instant::now() < deadline,
            "presenter never captured a frame"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(frame.width, W, "captured width");
    assert_eq!(frame.height, H, "captured height");
    assert_eq!(
        frame.pixel(W / 2, H / 2).expect("center pixel"),
        [R, G, B, A],
        "center pixel matches the color the real client drew after xdg/decoration negotiation",
    );

    // ---- 9. Shut down -------------------------------------------------------------------------------
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
            // Ack EVERY configure serial (the compositor sends several: new_toplevel, new_decoration,
            // set_mode) — a toolkit acks the latest it has seen. Map on the first.
            xdg_surface.ack_configure(serial);
            app.xdg_surface_configured = true;
            if !app.mapped {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.mapped = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &XdgToplevel,
        event: <XdgToplevel as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                app.toplevel_size = Some((width, height));
                app.toplevel_states = decode_states(&states);
            }
            xdg_toplevel::Event::ConfigureBounds { width, height } => {
                app.toplevel_bounds = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: <ZxdgToplevelDecorationV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            if let WEnum::Value(mode) = mode {
                app.decoration_mode = Some(mode);
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

// Objects whose events we don't act on.
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
ignore_dispatch!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    ZxdgDecorationManagerV1
);
