//! DEMO — `xdg_configure_ack_roundtrip`.
//!
//! The xdg-shell mapping handshake, asserted exactly end to end:
//!
//!   * On the first `wl_surface.commit` (no buffer) the compositor sends `xdg_surface.configure(serial)`
//!     paired with `xdg_toplevel.configure(width, height, states)`. A freshly-mapped, focused toplevel is
//!     `Activated` at the compositor's floating size. We assert the EXACT states array and size.
//!   * The client must `ack_configure(serial)` echoing the serial it received, then attach + commit. We
//!     assert content reaches the screen ONLY after the ack: before acking, no present exists; after
//!     ack + buffer + commit, the frame lands.
//!   * `set_maximized` drives a fresh configure with a NEW serial > the first, states
//!     `[Maximized, Activated]`, and the output's logical size. We ack that serial and prove the
//!     maximized content presents.
//!
//! Drives a real in-process wayland-client. PNGs are written for the mapped + maximized content.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::CapturedFrame;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

// xdg_toplevel.state enum discriminants (xdg-shell v1): maximized=1, fullscreen=2, resizing=3, activated=4.
const STATE_MAXIMIZED: u32 = 1;
const STATE_ACTIVATED: u32 = 4;

// The compositor's floating (INITIAL_TOPLEVEL_SIZE) and its primary output logical size (1920x1080 @ scale 1).
const FLOATING: (i32, i32) = (800, 600);
const OUTPUT_LOGICAL: (i32, i32) = (1920, 1080);

const W: i32 = 120;
const H: i32 = 90;
const MAP_COL: [u8; 4] = [0x18, 0xB8, 0xC0, 0xFF]; // teal — the first (mapped) buffer
const MAX_COL: [u8; 4] = [0xE0, 0x90, 0x10, 0xFF]; // orange — the post-maximize buffer

/// Decode an `xdg_toplevel.configure` states array (packed 32-bit native-endian enum values).
fn decode_states(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[derive(Default)]
struct App {
    /// Latest un-acked `xdg_surface.configure` serial.
    pending_serial: Option<u32>,
    /// Latest `xdg_toplevel.configure` size + decoded states.
    tl_size: Option<(i32, i32)>,
    tl_states: Vec<u32>,
    /// Bumped every time a fresh `xdg_surface.configure` arrives, so the test can await a NEW one.
    configure_count: u32,
}

fn present_with(
    caps: &std::sync::Arc<std::sync::Mutex<Vec<CapturedFrame>>>,
    col: [u8; 4],
) -> Option<CapturedFrame> {
    caps.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|f| f.width == W && f.height == H && f.pixel_is(W / 2, H / 2, col))
        .cloned()
}

#[test]
fn xdg_configure_ack_roundtrip() {
    let h = Harness::start("xdg_configure_ack_roundtrip");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let map_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "cfg-map",
        W,
        H,
        &solid(W, H, MAP_COL),
    );
    let max_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "cfg-max",
        W,
        H,
        &solid(W, H, MAX_COL),
    );

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-configure-ack".into());
    // First commit with NO buffer: this is what solicits the initial configure.
    surface.commit();

    let mut app = App::default();

    // ---- await the initial configure ----
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.configure_count < 1 {
        assert!(
            Instant::now() < deadline,
            "no initial xdg_surface.configure arrived"
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch initial configure");
    }
    let serial1 = app
        .pending_serial
        .expect("initial configure carried a serial");
    // EXACT initial configure: floating size + states == [Activated].
    assert_eq!(
        app.tl_size,
        Some(FLOATING),
        "initial configure size is the floating size"
    );
    assert_eq!(
        app.tl_states,
        vec![STATE_ACTIVATED],
        "a freshly-mapped focused toplevel is exactly Activated"
    );

    // Content must NOT be on screen before the ack: we have attached no buffer and not acked.
    assert!(
        present_with(&h.captures, MAP_COL).is_none(),
        "no content before ack_configure"
    );

    // ---- ack the exact serial, then attach + commit; content appears only now ----
    xdg.ack_configure(serial1);
    surface.attach(Some(&map_buffer), 0, 0);
    surface.damage(0, 0, W, H);
    surface.commit();

    let mapped = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(W / 2, H / 2, MAP_COL)
    })
    .expect("mapped content never presented after ack_configure");
    assert_eq!(
        mapped.pixel(W / 2, H / 2).unwrap(),
        MAP_COL,
        "mapped content color"
    );
    save_frame("xdg_configure_ack_roundtrip-mapped", &mapped);

    // ---- request maximize: a fresh configure with a new serial, [Maximized, Activated], output size ----
    let before = app.configure_count;
    toplevel.set_maximized();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.configure_count <= before {
        assert!(
            Instant::now() < deadline,
            "set_maximized produced no new configure"
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch maximize configure");
    }
    let serial2 = app
        .pending_serial
        .expect("maximize configure carried a serial");
    assert!(
        serial2 > serial1,
        "maximize configure serial ({serial2}) is newer than the initial ({serial1})"
    );
    assert_eq!(
        app.tl_size,
        Some(OUTPUT_LOGICAL),
        "maximize configure size is the output logical size"
    );
    let mut states = app.tl_states.clone();
    states.sort_unstable();
    assert_eq!(
        states,
        vec![STATE_MAXIMIZED, STATE_ACTIVATED],
        "maximized configure states are exactly Maximized + Activated"
    );

    // Ack the maximize serial (echoing exactly what we received), draw, and confirm the frame lands.
    xdg.ack_configure(serial2);
    surface.attach(Some(&max_buffer), 0, 0);
    surface.damage(0, 0, W, H);
    surface.commit();

    let maxed = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(W / 2, H / 2, MAX_COL)
    })
    .expect("maximized content never presented after ack_configure(serial2)");
    assert_eq!(
        maxed.pixel(W / 2, H / 2).unwrap(),
        MAX_COL,
        "maximized content color"
    );
    assert!(
        maxed.serial > mapped.serial,
        "maximized frame ({}) presents after the mapped frame ({})",
        maxed.serial,
        mapped.serial
    );
    save_frame("xdg_configure_ack_roundtrip-maximized", &maxed);

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        e: <XdgWmBase as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        _: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Record but DO NOT auto-ack — the test body drives ack_configure explicitly so the "content only
        // after ack" ordering is observable.
        if let xdg_surface::Event::Configure { serial } = e {
            app.pending_serial = Some(serial);
            app.configure_count += 1;
        }
    }
}
impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &XdgToplevel,
        e: <XdgToplevel as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = e
        {
            app.tl_size = Some((width, height));
            app.tl_states = decode_states(&states);
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer);
