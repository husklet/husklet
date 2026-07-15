//! DEMO — `shm_format_coverage`.
//!
//! A `wl_shm` buffer's four bytes-per-pixel can be laid out in several channel orders. This adapter's
//! read path ([`read_shm_rgba`](hl_compositor)) supports FOUR: `argb8888` / `xrgb8888` (the two Smithay
//! always advertises) and `abgr8888` / `xbgr8888` (R and B swapped — additionally advertised by the
//! adapter). This demo drives a real in-process wayland-client that, for EACH format, attaches a solid
//! buffer whose raw bytes encode the SAME logical color `(R,G,B,A) = (0x11, 0x22, 0x33, 0x44)` in that
//! format's own memory order, and asserts the COMPOSITED output is the exact expected RGBA:
//!
//!   * `argb8888` — memory `[B,G,R,A]`, alpha honoured  → captured `(0x11,0x22,0x33,0x44)`.
//!   * `xrgb8888` — memory `[B,G,R,X]`, opaque          → captured `(0x11,0x22,0x33,0xFF)`.
//!   * `abgr8888` — memory `[R,G,B,A]`, alpha honoured  → captured `(0x11,0x22,0x33,0x44)`.
//!   * `xbgr8888` — memory `[R,G,B,X]`, opaque          → captured `(0x11,0x22,0x33,0xFF)`.
//!
//! This proves channel-order handling (a wrong swizzle would surface `0x33` in the red slot) AND that the
//! `x`-formats treat the 4th byte as opaque while the `a`-formats honour it. A PNG per format is written.
//!
//! Formats NOT supported by the adapter's read path (e.g. 10-bit `xrgb2101010`, packed 16-bit `rgb565`,
//! or any planar/YUV format) are neither advertised nor decoded and are out of scope for this demo.

mod common;
use common::*;

use hl_compositor::adapter::smithay::CapturedFrame;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 32;
const H: i32 = 24;

// One logical color with four DISTINCT channels (so a wrong swizzle is visible) and a non-opaque alpha
// (so the x-vs-a distinction is visible).
const R: u8 = 0x11;
const G: u8 = 0x22;
const B: u8 = 0x33;
const A: u8 = 0x44;

/// A `W`×`H` solid canvas whose every pixel is the 4 bytes `pat`, in the buffer's own memory order.
fn solid_bytes(pat: [u8; 4]) -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..(W * H) {
        px.extend_from_slice(&pat);
    }
    px
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
}

/// Drive one format on a fresh client + surface, returning the composited frame.
fn run_format(h: &Harness, tag: &str, format: wl_shm::Format, bytes: Vec<u8>) -> CapturedFrame {
    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let buffer = make_buffer_fmt(&shm, &qh, &h.runtime_dir, tag, W, H, format, &bytes);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("demo-shm-format-{tag}"));
    surface.commit();

    let mut app = App { surface: surface.clone(), buffer: buffer.clone(), configured: false };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.configured {
        assert!(Instant::now() < deadline, "toplevel never configured for {tag}");
        queue.blocking_dispatch(&mut app).expect("dispatch configure");
    }

    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.height == H && f.pixel(1, 1).is_some()
    })
    .unwrap_or_else(|| panic!("frame never presented for {tag}"));

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(surface);
    std::mem::forget(buffer);
    frame
}

/// Assert every sampled pixel of `frame` equals `expect`.
fn assert_solid(frame: &CapturedFrame, expect: [u8; 4], tag: &str) {
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1), (W / 2, H / 2), (1, 1)] {
        assert_eq!(frame.pixel(x, y).unwrap(), expect, "{tag}: pixel ({x},{y}) exact RGBA");
    }
}

#[test]
fn shm_format_coverage() {
    let h = Harness::start("shm_format_coverage");

    // argb8888 — memory [B,G,R,A]; alpha honoured.
    let f = run_format(&h, "argb8888", wl_shm::Format::Argb8888, solid_bytes([B, G, R, A]));
    assert_solid(&f, [R, G, B, A], "argb8888");
    save_frame("shm_format_coverage-argb8888", &f);

    // xrgb8888 — memory [B,G,R,X]; 4th byte ignored → opaque.
    let f = run_format(&h, "xrgb8888", wl_shm::Format::Xrgb8888, solid_bytes([B, G, R, A]));
    assert_solid(&f, [R, G, B, 0xFF], "xrgb8888");
    save_frame("shm_format_coverage-xrgb8888", &f);

    // abgr8888 — memory [R,G,B,A] (R/B swapped vs argb); alpha honoured.
    let f = run_format(&h, "abgr8888", wl_shm::Format::Abgr8888, solid_bytes([R, G, B, A]));
    assert_solid(&f, [R, G, B, A], "abgr8888");
    save_frame("shm_format_coverage-abgr8888", &f);

    // xbgr8888 — memory [R,G,B,X]; 4th byte ignored → opaque.
    let f = run_format(&h, "xbgr8888", wl_shm::Format::Xbgr8888, solid_bytes([R, G, B, A]));
    assert_solid(&f, [R, G, B, 0xFF], "xbgr8888");
    save_frame("shm_format_coverage-xbgr8888", &f);

    h.shutdown();
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
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel);
