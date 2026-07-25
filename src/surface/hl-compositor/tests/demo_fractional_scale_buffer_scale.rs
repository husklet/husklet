//! DEMO — `fractional_scale_buffer_scale`.
//!
//! Two HiDPI surface-scaling mechanisms, asserted exactly:
//!
//!   * `wp_fractional_scale_manager_v1`: a client creates a `wp_fractional_scale_v1` for its surface and
//!     the compositor answers with `preferred_scale` — the scale, as `round(scale × 120)`, the client
//!     should render at. We assert the EXACT wire value (120 == scale 1.0, sourced from the output).
//!   * `wl_surface.set_buffer_scale`: an integer buffer scale means the on-screen LOGICAL size is
//!     `buffer_pixels / scale`. We attach a 240×240 buffer with `set_buffer_scale(2)` and assert the
//!     presented pixel size is EXACTLY 120×120 logical while the backing buffer stays 240×240.
//!
//! Drives a real in-process wayland-client. A PNG of the presented buffer is written for confirmation.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
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

const BUF: i32 = 240; // backing buffer is 240x240 device pixels
const SCALE: i32 = 2; // wl_surface.set_buffer_scale
const LOGICAL: i32 = 120; // => on-screen logical size 240/2 = 120x120
const EXPECT_PREFERRED: u32 = 120; // preferred_scale == round(1.0 * 120), output scale is 1
const COL: [u8; 4] = [0x18, 0xC0, 0x98, 0xFF]; // teal

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
    preferred: Option<u32>,
}

#[test]
fn fractional_scale_buffer_scale() {
    let h = Harness::start("fractional_scale_buffer_scale");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // The corner under test: the adapter must ADVERTISE the fractional-scale manager.
    let frac_mgr: WpFractionalScaleManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("wp_fractional_scale_manager_v1 (adapter must advertise it)");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "fs",
        BUF,
        BUF,
        &solid(BUF, BUF, COL),
    );

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-fractional-scale".into());
    // Creating the fractional-scale object solicits the compositor's preferred_scale.
    let _frac: WpFractionalScaleV1 = frac_mgr.get_fractional_scale(&surface, &qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        configured: false,
        preferred: None,
    };

    // Await both the configure (drives the map) and the preferred_scale event.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.configured && app.preferred.is_some()) {
        assert!(
            Instant::now() < deadline,
            "no configure / preferred_scale (configured={}, preferred={:?})",
            app.configured,
            app.preferred
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch configure + preferred_scale");
    }

    // EXACT preferred fractional scale on the wire.
    assert_eq!(
        app.preferred,
        Some(EXPECT_PREFERRED),
        "preferred_scale == round(output_scale * 120)"
    );

    // The presented frame: raw buffer 240x240, but logical (on-screen) size 120x120 from buffer scale 2.
    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == BUF && f.height == BUF && f.pixel_is(BUF / 2, BUF / 2, COL)
    })
    .expect("scaled buffer never presented");
    assert_eq!(
        (frame.width, frame.height),
        (BUF, BUF),
        "backing buffer stays 240x240 device pixels"
    );
    assert_eq!(
        (frame.logical_width, frame.logical_height),
        (LOGICAL, LOGICAL),
        "presented LOGICAL size is buffer/scale == 120x120 (scale {SCALE})",
    );
    assert_eq!(
        frame.pixel(BUF / 2, BUF / 2).unwrap(),
        COL,
        "presented content color"
    );

    save_frame("fractional_scale_buffer_scale-presented", &frame);

    h.shutdown();
    let _ = toplevel;
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &WpFractionalScaleV1,
        e: <WpFractionalScaleV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e {
            app.preferred = Some(scale);
        }
    }
}
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
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.configured {
                app.surface.set_buffer_scale(SCALE);
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, BUF, BUF);
                app.surface.commit();
                app.configured = true;
            }
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
ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WpFractionalScaleManagerV1,
    XdgToplevel
);
