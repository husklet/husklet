//! DEMO — `transform_viewport_compose`.
//!
//! `wl_surface.set_buffer_transform` and `wp_viewport` (crop + scale) must COMPOSE, not clobber each
//! other. Wayland's buffer→surface chain is: (1) the buffer transform + buffer scale map the buffer into
//! SURFACE space (a 90°/270° rotation swaps width/height), then (2) the `wp_viewport` src crop — stated in
//! surface coordinates — selects a region that (3) `set_destination` scales to size the surface. So a
//! rotated buffer must un-rotate FIRST, and only then does the crop apply in the upright surface space.
//!
//! This demo drives a real in-process wayland-client that paints an 80×40 buffer as four quadrants laid
//! out so that AFTER a 90° transform the upright 40×80 surface reads RED(top-left) GREEN(top-right)
//! BLUE(bottom-left) YELLOW(bottom-right). It then crops the surface-space bottom half
//! `set_source(0,40,40,40)` and upscales it 2× `set_destination(80,80)`. The exact-correct composited
//! output is an 80×80 image: left half BLUE, right half YELLOW, with NO red or green (cropped away).
//!
//! This distinguishes the composed path from the old "viewport wins, transform ignored" behaviour: if the
//! crop were applied to the RAW (un-rotated) 80×40 buffer, `set_source(0,40,…)` clamps to the last buffer
//! row (buffer is only 40 tall) and yields a flat YELLOW image — no blue. The blue/yellow split is proof
//! the rotation happened before the crop. A PNG of the presented frame is written for confirmation.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_output::Transform, wl_registry::WlRegistry,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const BUF_W: i32 = 80;
const BUF_H: i32 = 40;
const CROP: (f64, f64, f64, f64) = (0.0, 40.0, 40.0, 40.0); // surface-space bottom half (upright 40×80)
const DST: (i32, i32) = (80, 80); // 2× upscale of the 40×40 crop

const RED: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF];
const GREEN: [u8; 4] = [0x20, 0xD0, 0x20, 0xFF];
const BLUE: [u8; 4] = [0x20, 0x30, 0xE0, 0xFF];
const YELLOW: [u8; 4] = [0xE0, 0xD0, 0x10, 0xFF];

/// Paint the source buffer so that AFTER a 90° buffer transform the upright 40×80 surface reads
/// RED(TL) GREEN(TR) BLUE(BL) YELLOW(BR). The 90° map sends buffer `(bx,by)` → surface `(by, 79-bx)`,
/// so these buffer regions land in those surface quadrants (verified in the module doc).
fn source_buffer() -> Vec<u8> {
    let mut px = solid(BUF_W, BUF_H, [0, 0, 0, 0xFF]);
    fill_rect(&mut px, BUF_W, BUF_H, 40, 0, 40, 20, RED); // → surface top-left
    fill_rect(&mut px, BUF_W, BUF_H, 40, 20, 40, 20, GREEN); // → surface top-right
    fill_rect(&mut px, BUF_W, BUF_H, 0, 0, 40, 20, BLUE); // → surface bottom-left
    fill_rect(&mut px, BUF_W, BUF_H, 0, 20, 40, 20, YELLOW); // → surface bottom-right
    px
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    viewport: WpViewport,
    configured: bool,
}

#[test]
fn transform_viewport_compose() {
    let h = Harness::start("transform_viewport_compose");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let viewporter: WpViewporter = globals.bind(&qh, 1..=1, ()).expect("wp_viewporter");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "tvc", BUF_W, BUF_H, &source_buffer());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-transform-viewport-compose".into());
    let viewport = viewporter.get_viewport(&surface, &qh, ());
    surface.commit();

    let mut app = App { surface: surface.clone(), buffer: buffer.clone(), viewport: viewport.clone(), configured: false };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.configured {
        assert!(Instant::now() < deadline, "toplevel never configured");
        queue.blocking_dispatch(&mut app).expect("dispatch configure");
    }

    // The composed frame is the DST-sized cropped+rotated image; a blue top-left proves both applied.
    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == DST.0 && f.height == DST.1 && f.pixel_is(0, 0, BLUE)
    })
    .expect("composed (rotated+cropped) frame never presented");

    assert_eq!((frame.width, frame.height), DST, "presented size is the viewport destination");
    assert_eq!((frame.logical_width, frame.logical_height), DST, "logical size is the viewport destination");

    // Left half BLUE, right half YELLOW (crop of the upright surface's bottom half, scaled 2×).
    assert_eq!(frame.pixel(0, 0).unwrap(), BLUE, "top-left = surface bottom-left (blue)");
    assert_eq!(frame.pixel(0, DST.1 - 1).unwrap(), BLUE, "bottom-left = surface bottom-left (blue)");
    assert_eq!(frame.pixel(20, DST.1 / 2).unwrap(), BLUE, "left interior = blue");
    assert_eq!(frame.pixel(DST.0 - 1, 0).unwrap(), YELLOW, "top-right = surface bottom-right (yellow)");
    assert_eq!(frame.pixel(DST.0 - 1, DST.1 - 1).unwrap(), YELLOW, "bottom-right = surface bottom-right (yellow)");
    assert_eq!(frame.pixel(60, DST.1 / 2).unwrap(), YELLOW, "right interior = yellow");

    // The cropped-away top half (red + green) must not survive anywhere.
    for y in 0..frame.height {
        for x in 0..frame.width {
            let p = frame.pixel(x, y).unwrap();
            assert_ne!(p, RED, "cropped-away red leaked at ({x},{y})");
            assert_ne!(p, GREEN, "cropped-away green leaked at ({x},{y})");
        }
    }

    save_frame("transform_viewport_compose-presented", &frame);

    h.shutdown();
    let _ = toplevel;
}

// ---------- Dispatch plumbing ----------

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
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.configured {
                // Declare the buffer transform AND the viewport crop+scale, then attach + commit.
                app.surface.set_buffer_transform(Transform::_90);
                app.viewport.set_source(CROP.0, CROP.1, CROP.2, CROP.3);
                app.viewport.set_destination(DST.0, DST.1);
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, BUF_W, BUF_H);
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WpViewporter, WpViewport, XdgToplevel);
