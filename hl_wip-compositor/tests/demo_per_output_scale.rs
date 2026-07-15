//! DEMO — `per_output_scale`.
//!
//! A surface's preferred render scale is a property of the OUTPUT it is displayed on: a HiDPI (scale-2)
//! monitor wants twice the pixel density of a scale-1 one. This demo stands the compositor up on two
//! outputs — A `1920×1080@0,0` scale 1, B `2560×1440@1920,0` scale 2 — creates a `wp_fractional_scale_v1`
//! on a mapped surface, and asserts the EXACT `preferred_scale` the compositor sends changes with the
//! output the surface is on:
//!
//!   * on the scale-1 output A the surface's preferred scale is `120` (== round(1.0 × 120));
//!   * routing it (by position) onto the scale-2 output B re-sends `240` (== round(2.0 × 120)) — DISTINCT
//!     from the scale-1 value, and driven purely by the output's scale.
//!
//! The route is driven through the host/window-manager seam (`InputCommand::MoveToplevelToPoint`), which
//! re-emits the surface's preferred fractional scale from its new output. No PNG: the evidence is the exact
//! `preferred_scale` wire values.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_registry::WlRegistry,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 100;
const H: i32 = 100;
const COL: [u8; 4] = [0xC0, 0x40, 0x80, 0xFF];
const SCALE1_PREFERRED: u32 = 120; // round(1.0 * 120)
const SCALE2_PREFERRED: u32 = 240; // round(2.0 * 120)

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    /// `preferred_scale` values in arrival order.
    preferred: Vec<u32>,
}

#[test]
fn per_output_scale() {
    std::env::set_var("HL_OUTPUTS", "1920x1080@0,0;2560x1440@1920,0*2");
    let h = Harness::start("per_output_scale");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let frac_mgr: WpFractionalScaleManagerV1 =
        globals.bind(&qh, 1..=1, ()).expect("wp_fractional_scale_manager_v1");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "pos", W, H, &solid(W, H, COL));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-per-output-scale".into());
    // Creating the fractional-scale object solicits the compositor's preferred_scale for the surface's
    // CURRENT output (the primary, scale 1).
    let _frac: WpFractionalScaleV1 = frac_mgr.get_fractional_scale(&surface, &qh, ());
    surface.commit();

    let mut app = App { surface: surface.clone(), buffer: buffer.clone(), drawn: false, frame_done: false, preferred: Vec::new() };

    // Map + receive the initial preferred_scale (from scale-1 output A).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done && !app.preferred.is_empty()) {
        assert!(Instant::now() < deadline, "no map / initial preferred_scale (preferred={:?})", app.preferred);
        queue.blocking_dispatch(&mut app).expect("dispatch map + preferred_scale");
    }
    assert_eq!(app.preferred, vec![SCALE1_PREFERRED], "on scale-1 output A, preferred_scale is 120");

    // ---- route the surface onto the scale-2 output B (by a point inside its region) ----
    h.input_tx
        .send(InputCommand::MoveToplevelToPoint { index: 0, x: 2020, y: 100 })
        .expect("route to B");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.preferred.len() < 2 {
        assert!(Instant::now() < deadline, "no re-sent preferred_scale after routing to B (preferred={:?})", app.preferred);
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.preferred,
        vec![SCALE1_PREFERRED, SCALE2_PREFERRED],
        "on scale-2 output B, preferred_scale becomes 240 — DISTINCT from the scale-1 value",
    );
    assert_ne!(SCALE1_PREFERRED, SCALE2_PREFERRED, "the two outputs' preferred scales differ");

    h.shutdown();
    let _ = toplevel;
    let _ = xdg;
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(app: &mut Self, _: &WpFractionalScaleV1, e: <WpFractionalScaleV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e {
            app.preferred.push(scale);
        }
    }
}
impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(_: &mut Self, wm: &XdgWmBase, e: <XdgWmBase as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.drawn {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for App {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.frame_done = true;
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WpFractionalScaleManagerV1, XdgToplevel);
