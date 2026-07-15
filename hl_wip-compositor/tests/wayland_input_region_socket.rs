//! Live-socket INPUT-REGION proof: `wl_surface.set_input_region` actually gates pointer hit-testing.
//!
//! `wayland_input_socket` proved injected pointer/keyboard input reaches a focused client, but it never
//! exercised `wl_surface.set_input_region` — a request a real toolkit relies on (GTK excludes its CSD
//! shadow from input; overlay surfaces set an EMPTY region to be click-through). Smithay stores the region
//! for free, but the adapter must READ it and feed it to the neutral scene, whose `surface_at` /
//! `accepts_input_at` gate pointer focus on it. Without that wiring the request is silently dropped and
//! every surface accepts input over its whole rectangle. This test locks the real effect end to end:
//!
//!   1. A real client maps a `W`×`H` toplevel and sets an input region covering only the surface's RIGHT
//!      half (`x ∈ [W/2, W)`), then commits it.
//!   2. The test injects pointer motion to a point in the LEFT half (inside the surface bounds but OUTSIDE
//!      the input region) and asserts, after several dispatch cycles, that the client received NO
//!      `wl_pointer.enter` — the point is not input-sensitive.
//!   3. The test then injects motion to a point in the RIGHT half (inside the region) and asserts the
//!      client DOES receive `wl_pointer.enter`, naming its surface at the correct surface-local coordinate.
//!
//! Step 2 is the discriminating assertion: on the un-wired (silently-dropped) code the left-half point
//! WOULD enter. Fully headless — real socket, real wire, real seat. Its own binary because it mutates
//! process-global `$XDG_RUNTIME_DIR` / `$WAYLAND_DISPLAY`.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{self, input_channel, InputCommand, PngPresenter};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_output::WlOutput,
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{Capability, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

// A non-square surface so an axis mix-up would be caught.
const W: i32 = 200;
const H: i32 = 150;
// The color the client paints. `wl_shm` Argb8888 is 32-bit little-endian → memory bytes `[B, G, R, A]`.
const R: u8 = 0x22;
const G: u8 = 0x55;
const B: u8 = 0x99;
const A: u8 = 0xFF;

// The input region: the surface's RIGHT half only. `[W/2, W) × [0, H)`.
const REGION_X: i32 = W / 2; // 100
const REGION_W: i32 = W - REGION_X; // 100

// A point in the LEFT half — inside the surface bounds, OUTSIDE the input region. Must NOT enter.
const OUT_X: f64 = 40.0;
const OUT_Y: f64 = 75.0;
// A point in the RIGHT half — inside the input region. Must enter at this exact surface-local coordinate.
const IN_X: f64 = 150.0;
const IN_Y: f64 = 90.0;

struct AppData {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
    released: bool,
    frame_done: bool,
    seat_caps: Option<Capability>,
    /// `wl_pointer.enter`: `(surface matched ours, surface-local x, surface-local y)`.
    pointer_enter: Option<(bool, f64, f64)>,
}

#[test]
fn set_input_region_gates_pointer_hit_testing() {
    // ---- 1. Private XDG_RUNTIME_DIR + compositor with a host input channel ----------------------------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-inputregion-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET");

    let png_dir = runtime_dir.join("png");
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(png_dir.clone());
    let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();
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
    let socket_path = runtime_dir.join(&socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(Instant::now() < deadline, "discovery socket {socket_path:?} never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 2. Connect a real client, bind globals, build the toplevel -----------------------------------
    let conn = Connection::connect_to_env().expect("connect_to_env failed");
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor global");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm global");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base global");
    let _output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output global");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat global");

    // Build a wl_shm buffer of the known color/size.
    let stride = W * 4;
    let size = (stride * H) as usize;
    let mut pixels = Vec::with_capacity(size);
    for _ in 0..(W * H) {
        pixels.extend_from_slice(&[B, G, R, A]);
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

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("hl-wip-input-region".into());

    // Restrict input to the RIGHT half of the surface. The region is committed with the surface below (in
    // the xdg_surface Configure handler), so it is applied atomically with the first buffer.
    let region: WlRegion = compositor.create_region(&qh, ());
    region.add(REGION_X, 0, REGION_W, H);
    surface.set_input_region(Some(&region));

    surface.commit(); // initial empty commit → first configure

    let mut app = AppData {
        surface: surface.clone(),
        buffer: buffer.clone(),
        configured: false,
        released: false,
        frame_done: false,
        seat_caps: None,
        pointer_enter: None,
    };

    // Drive the map handshake to completion so the server surface has committed content (+ input region).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.configured && app.released && app.frame_done) {
        assert!(
            Instant::now() < deadline,
            "map handshake incomplete: configured={} released={} frame_done={}",
            app.configured, app.released, app.frame_done,
        );
        queue.blocking_dispatch(&mut app).expect("client dispatch (map)");
    }

    // Seat must advertise pointer; create it and roundtrip so the server has registered it before inject.
    queue.roundtrip(&mut app).expect("roundtrip for seat caps");
    let caps = app.seat_caps.expect("seat capabilities");
    assert!(caps.contains(Capability::Pointer), "seat advertises pointer, got {caps:?}");
    let pointer: WlPointer = seat.get_pointer(&qh, ());
    queue.roundtrip(&mut app).expect("roundtrip after creating pointer");
    assert!(pointer.is_alive(), "pointer object alive");

    // ---- 3. Move OUTSIDE the input region (left half) → the client must NOT enter ---------------------
    input_tx.send(InputCommand::FocusTopmostKeyboard).expect("send focus");
    input_tx.send(InputCommand::PointerMotion { x: OUT_X, y: OUT_Y }).expect("send outside motion");

    // Give the compositor + wire ample cycles to deliver anything it was going to deliver. With input
    // region wired, a point outside it produces no `wl_pointer.enter`.
    let settle = Instant::now() + Duration::from_millis(600);
    while Instant::now() < settle {
        queue.roundtrip(&mut app).expect("client dispatch (outside)");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        app.pointer_enter.is_none(),
        "pointer entered at a point OUTSIDE the input region — set_input_region was not honored: {:?}",
        app.pointer_enter,
    );

    // ---- 4. Move INSIDE the input region (right half) → the client DOES enter --------------------------
    input_tx.send(InputCommand::PointerMotion { x: IN_X, y: IN_Y }).expect("send inside motion");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.pointer_enter.is_none() {
        assert!(Instant::now() < deadline, "pointer never entered at a point INSIDE the input region");
        queue.roundtrip(&mut app).expect("client dispatch (inside)");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (matched, ex, ey) = app.pointer_enter.unwrap();
    assert!(matched, "wl_pointer.enter named a different surface than the client's toplevel");
    assert_eq!((ex, ey), (IN_X, IN_Y), "wl_pointer.enter surface-local coordinate (inside the region)");

    // ---- 5. Shut down --------------------------------------------------------------------------------
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

impl Dispatch<WlSeat, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(caps) = capabilities {
                app.seat_caps = Some(caps);
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        event: <WlPointer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } = event {
            app.pointer_enter = Some((surface.id() == app.surface.id(), surface_x, surface_y));
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
ignore_dispatch!(WlCompositor, WlSurface, WlShm, WlShmPool, XdgToplevel, WlOutput, WlRegion);
