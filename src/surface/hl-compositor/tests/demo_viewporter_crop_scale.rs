//! DEMO — `viewporter_crop_scale`.
//!
//! `wp_viewporter` lets a client crop a source rectangle out of its buffer and scale it to a destination
//! logical size WITHOUT re-rendering (video players crop letterboxing; browsers scale). This demo drives a
//! real in-process wayland-client that paints a 120×120 buffer with four distinctly-colored 4×4 corner
//! markers INSIDE a 60×60 crop window (and a magenta marker OUTSIDE it that must be cropped away), then
//! sets `wp_viewport.set_source(30,30,60,60)` + `set_destination(120,120)` (a 2× upscale of the crop).
//!
//! We assert the presented frame is EXACTLY the cropped 60×60 region scaled to 120×120: pixel-exact
//! corners (red / green / blue / yellow), a white center, the interior background gray, and NO trace of
//! the out-of-crop magenta. A PNG of the presented (cropped+scaled) frame is written for confirmation.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const BUF: i32 = 120; // source buffer is 120x120
const CROP: (i32, i32, i32, i32) = (30, 30, 60, 60); // source crop window
const DST: (i32, i32) = (120, 120); // destination logical size (2x upscale of the crop)

const GRAY: [u8; 4] = [0x40, 0x40, 0x40, 0xFF];
const RED: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF];
const GREEN: [u8; 4] = [0x20, 0xD0, 0x20, 0xFF];
const BLUE: [u8; 4] = [0x20, 0x30, 0xE0, 0xFF];
const YELLOW: [u8; 4] = [0xE0, 0xD0, 0x10, 0xFF];
const WHITE: [u8; 4] = [0xF0, 0xF0, 0xF0, 0xFF];
const MAGENTA: [u8; 4] = [0xD0, 0x10, 0xC0, 0xFF]; // OUTSIDE the crop — must not survive

/// Paint the source buffer: gray fill, a magenta marker OUTSIDE the crop at (0,0), and inside the crop
/// window four colored corners + a white center.
fn source_buffer() -> Vec<u8> {
    let mut px = solid(BUF, BUF, GRAY);
    fill_rect(&mut px, BUF, BUF, 0, 0, 4, 4, MAGENTA); // outside the (30,30,60,60) crop
                                                       // Corners of the crop window x in [30,90), y in [30,90): last in-crop index is 89.
    fill_rect(&mut px, BUF, BUF, 30, 30, 4, 4, RED); // top-left of crop
    fill_rect(&mut px, BUF, BUF, 86, 30, 4, 4, GREEN); // top-right of crop (cols 86..89)
    fill_rect(&mut px, BUF, BUF, 30, 86, 4, 4, BLUE); // bottom-left of crop
    fill_rect(&mut px, BUF, BUF, 86, 86, 4, 4, YELLOW); // bottom-right of crop
    fill_rect(&mut px, BUF, BUF, 58, 58, 4, 4, WHITE); // center of crop
    px
}

struct App {
    surface: WlSurface,
    src_buffer: WlBuffer,
    viewport: WpViewport,
    configured: bool,
}

#[test]
fn viewporter_crop_scale() {
    let h = Harness::start("viewporter_crop_scale");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // The corner under test: the adapter must ADVERTISE wp_viewporter for this to bind.
    let viewporter: WpViewporter = globals
        .bind(&qh, 1..=1, ())
        .expect("wp_viewporter (adapter must advertise it)");

    let src_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "vp-src",
        BUF,
        BUF,
        &source_buffer(),
    );

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-viewporter".into());
    let viewport = viewporter.get_viewport(&surface, &qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        src_buffer: src_buffer.clone(),
        viewport: viewport.clone(),
        configured: false,
    };

    // Await the initial configure (the App handler acks + sets source/destination + attaches on it).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.configured {
        assert!(Instant::now() < deadline, "toplevel never configured");
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch configure");
    }

    // The presented frame must be the cropped+scaled 120x120 image (red top-left corner proves the crop).
    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == DST.0 && f.height == DST.1 && f.pixel_is(0, 0, RED)
    })
    .expect("cropped+scaled frame never presented");

    // EXACT presented pixel size == the destination logical size.
    assert_eq!(
        (frame.width, frame.height),
        DST,
        "presented size is the viewport destination"
    );
    assert_eq!(
        (frame.logical_width, frame.logical_height),
        DST,
        "logical size is the viewport destination"
    );

    // Pixel-exact corners of the cropped+scaled region.
    assert_eq!(
        frame.pixel(0, 0).unwrap(),
        RED,
        "top-left corner == crop TL (red)"
    );
    assert_eq!(
        frame.pixel(DST.0 - 1, 0).unwrap(),
        GREEN,
        "top-right corner == crop TR (green)"
    );
    assert_eq!(
        frame.pixel(0, DST.1 - 1).unwrap(),
        BLUE,
        "bottom-left corner == crop BL (blue)"
    );
    assert_eq!(
        frame.pixel(DST.0 - 1, DST.1 - 1).unwrap(),
        YELLOW,
        "bottom-right corner == crop BR (yellow)"
    );
    // Center of the crop scales to the center of the destination.
    assert_eq!(
        frame.pixel(DST.0 / 2, DST.1 / 2).unwrap(),
        WHITE,
        "center == crop center (white)"
    );
    // Interior background between markers is the crop's gray fill.
    assert_eq!(
        frame.pixel(30, 30).unwrap(),
        GRAY,
        "interior background is the crop gray"
    );

    // The out-of-crop magenta must have been cropped away entirely.
    for y in 0..frame.height {
        for x in 0..frame.width {
            assert_ne!(
                frame.pixel(x, y).unwrap(),
                MAGENTA,
                "out-of-crop magenta leaked at ({x},{y})"
            );
        }
    }

    save_frame("viewporter_crop_scale-presented", &frame);

    h.shutdown();
    let _ = toplevel;
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
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.configured {
                // Crop the source rect and scale it to the destination, then attach + commit.
                app.viewport
                    .set_source(CROP.0 as f64, CROP.1 as f64, CROP.2 as f64, CROP.3 as f64);
                app.viewport.set_destination(DST.0, DST.1);
                app.surface.attach(Some(&app.src_buffer), 0, 0);
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
    WpViewporter,
    WpViewport,
    XdgToplevel
);
