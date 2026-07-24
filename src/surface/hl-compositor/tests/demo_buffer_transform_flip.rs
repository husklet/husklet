//! DEMO — `buffer_transform_flip`.
//!
//! The FLIPPED `wl_surface.set_buffer_transform` values mirror a buffer (optionally combined with a
//! rotation) before it is presented. This demo drives a real in-process wayland-client that attaches a
//! non-square buffer with four distinctly-colored corner markers and sets a flipped transform, then
//! asserts the composited output is EXACTLY mirrored:
//!
//!   * `Flipped` — a horizontal mirror (around the vertical axis): left/right corners swap, top/bottom
//!     stay. Presented size stays BUF_W × BUF_H.
//!   * `Flipped_90` — flip then rotate 90° counter-clockwise: presented size is swapped (BUF_H × BUF_W)
//!     and each corner lands where the composed flip+rotate dictates.
//!
//! Corner markers: RED = buffer top-left, GREEN = buffer top-right, BLUE = buffer bottom-left,
//! YELLOW = buffer bottom-right. The exact per-transform corner mapping is asserted pixel-exact and
//! confirmed from the written PNG.

mod client_harness;
use client_harness::*;

use hl_compositor::adapter::smithay::CapturedFrame;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_output::Transform,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const BUF_W: i32 = 80;
const BUF_H: i32 = 40;
const M: i32 = 6; // corner marker size

const RED: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF]; // buffer top-left
const GREEN: [u8; 4] = [0x20, 0xD0, 0x20, 0xFF]; // buffer top-right
const BLUE: [u8; 4] = [0x20, 0x30, 0xE0, 0xFF]; // buffer bottom-left
const YELLOW: [u8; 4] = [0xE0, 0xD0, 0x10, 0xFF]; // buffer bottom-right
const GRAY: [u8; 4] = [0x30, 0x30, 0x30, 0xFF]; // background

fn source_buffer() -> Vec<u8> {
    let mut px = solid(BUF_W, BUF_H, GRAY);
    fill_rect(&mut px, BUF_W, BUF_H, 0, 0, M, M, RED); // top-left
    fill_rect(&mut px, BUF_W, BUF_H, BUF_W - M, 0, M, M, GREEN); // top-right
    fill_rect(&mut px, BUF_W, BUF_H, 0, BUF_H - M, M, M, BLUE); // bottom-left
    fill_rect(&mut px, BUF_W, BUF_H, BUF_W - M, BUF_H - M, M, M, YELLOW); // bottom-right
    px
}

fn corners(f: &CapturedFrame) -> ([u8; 4], [u8; 4], [u8; 4], [u8; 4]) {
    let (w, h) = (f.width, f.height);
    let tl = f.pixel(2, 2).unwrap();
    let tr = f.pixel(w - 3, 2).unwrap();
    let bl = f.pixel(2, h - 3).unwrap();
    let br = f.pixel(w - 3, h - 3).unwrap();
    (tl, tr, bl, br)
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    transform: Transform,
    configured: bool,
}

fn run_transform(h: &Harness, tag: &str, transform: Transform, ew: i32, eh: i32) -> CapturedFrame {
    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        tag,
        BUF_W,
        BUF_H,
        &source_buffer(),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("demo-buffer-transform-{tag}"));
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        transform,
        configured: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.configured {
        assert!(
            Instant::now() < deadline,
            "toplevel never configured for {tag}"
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch configure");
    }

    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, move |f| {
        f.width == ew && f.height == eh && f.pixel(2, 2).is_some()
    })
    .unwrap_or_else(|| panic!("flipped frame never presented for {tag}"));

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(surface);
    std::mem::forget(buffer);
    frame
}

#[test]
fn buffer_transform_flip() {
    let h = Harness::start("buffer_transform_flip");

    // ---- Flipped : horizontal mirror, presented size unchanged (BUF_W × BUF_H) ----
    // buffer TL(RED)->surface TR, TR(GREEN)->TL, BL(BLUE)->BR, BR(YELLOW)->BL.
    let ff = run_transform(&h, "flip", Transform::Flipped, BUF_W, BUF_H);
    assert_eq!(
        (ff.width, ff.height),
        (BUF_W, BUF_H),
        "flip keeps presented dimensions"
    );
    let (tl, tr, bl, br) = corners(&ff);
    assert_eq!(
        tl, GREEN,
        "flip: surface top-left = buffer top-right (green) — mirrored L/R"
    );
    assert_eq!(tr, RED, "flip: surface top-right = buffer top-left (red)");
    assert_eq!(
        bl, YELLOW,
        "flip: surface bottom-left = buffer bottom-right (yellow)"
    );
    assert_eq!(
        br, BLUE,
        "flip: surface bottom-right = buffer bottom-left (blue)"
    );
    // Top row stays top, bottom row stays bottom (a pure horizontal mirror, not a rotation).
    assert_eq!(tl, GREEN);
    assert_eq!(bl, YELLOW);
    save_frame("buffer_transform_flip-flipped", &ff);

    // ---- Flipped_90 : flip then rotate 90° CCW, presented size swapped (BUF_H × BUF_W) ----
    // buffer TL(RED)->surface BR, TR(GREEN)->TR, BL(BLUE)->BL, BR(YELLOW)->TL.
    let ff90 = run_transform(&h, "flip90", Transform::Flipped90, BUF_H, BUF_W);
    assert_eq!(
        (ff90.width, ff90.height),
        (BUF_H, BUF_W),
        "flip_90 swaps presented dimensions"
    );
    assert_eq!(
        (ff90.logical_width, ff90.logical_height),
        (BUF_H, BUF_W),
        "flip_90 swaps logical size"
    );
    let (tl, tr, bl, br) = corners(&ff90);
    assert_eq!(
        tl, YELLOW,
        "flip_90: surface top-left = buffer bottom-right (yellow)"
    );
    assert_eq!(
        tr, GREEN,
        "flip_90: surface top-right = buffer top-right (green)"
    );
    assert_eq!(
        bl, BLUE,
        "flip_90: surface bottom-left = buffer bottom-left (blue)"
    );
    assert_eq!(
        br, RED,
        "flip_90: surface bottom-right = buffer top-left (red)"
    );
    save_frame("buffer_transform_flip-flipped90", &ff90);

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
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.configured {
                app.surface.set_buffer_transform(app.transform);
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
ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    XdgToplevel
);
