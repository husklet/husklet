//! DEMO — `single_pixel_buffer` (`wp_single_pixel_buffer_v1` — 1×1 solid-color buffers).
//!
//! Chrome/Ozone and video players attach a `wp_single_pixel_buffer_v1` buffer (a 4-channel color, no shm
//! pool, no fd) for solid-color quads — window backgrounds, letterbox bars — and pair it with a
//! `wp_viewport` to scale that single pixel up to fill the surface. This demo drives a real in-process
//! client that creates a 1×1 opaque single-pixel buffer, scales it to the full surface with a viewport,
//! commits, and asserts the composited frame is EXACTLY that solid color across its whole area — proving
//! the adapter's newly-wired single-pixel global genuinely turns the color into presented pixels, not just
//! that the global binds.

mod common;
use common::*;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 90;
/// The solid color the single-pixel buffer carries (RGBA, opaque).
const COLOR: [u8; 4] = [0x20, 0xC0, 0x60, 0xFF];

/// Encode an 8-bit channel value into the 32-bit-per-channel wire value `create_u32_rgba_buffer` takes, so
/// the compositor's `rgba8888()` (which divides by `u32::MAX / 255`) recovers exactly this 8-bit value.
fn ch(v: u8) -> u32 {
    v as u32 * (u32::MAX / 255)
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    viewport: WpViewport,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn single_pixel_buffer_scales_to_solid_fill() {
    let h = Harness::start("single_pixel_buffer");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let _shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // The newly-wired globals under test.
    let spb: WpSinglePixelBufferManagerV1 =
        globals.bind(&qh, 1..=1, ()).expect("wp_single_pixel_buffer_manager_v1 advertised");
    let viewporter: WpViewporter = globals.bind(&qh, 1..=1, ()).expect("wp_viewporter advertised");

    // A 1×1 opaque solid-color buffer — no shm pool, no fd.
    let buffer = spb.create_u32_rgba_buffer(ch(COLOR[0]), ch(COLOR[1]), ch(COLOR[2]), ch(COLOR[3]), &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-single-pixel".to_string());
    // Scale the 1×1 buffer up to the full surface: crop the whole 1×1 source, scale to W×H.
    let viewport = viewporter.get_viewport(&surface, &qh, ());
    surface.commit();

    let mut app = App { surface: surface.clone(), buffer, viewport, drawn: false, frame_done: false };

    // The presented frame must be W×H and a pure solid fill of COLOR — center and every corner.
    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.height == H && f.pixel_is(W / 2, H / 2, COLOR)
    })
    .expect("single-pixel buffer never composited to the scaled solid fill");
    assert_eq!((frame.width, frame.height), (W, H), "presented size is the viewport destination");
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1), (W / 2, H / 2)] {
        assert_eq!(
            frame.pixel(x, y).unwrap(),
            COLOR,
            "single-pixel color fills ({x},{y}) exactly"
        );
    }
    save_frame("single_pixel_buffer-presented", &frame);

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(_: &mut Self, wm: &XdgWmBase, e: <XdgWmBase as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e { wm.pong(serial); }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.drawn {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                // Whole 1×1 source, upscaled to the full W×H surface.
                app.viewport.set_source(0.0, 0.0, 1.0, 1.0);
                app.viewport.set_destination(W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for App {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e { app.frame_done = true; }
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel, WpSinglePixelBufferManagerV1, WpViewporter, WpViewport);
