//! DEMO — `buffer_transform_rotate`.
//!
//! `wl_surface.set_buffer_transform` lets a client render its content pre-rotated (e.g. for a display the
//! compositor has rotated, so the buffer can be scanned out directly). The compositor applies the INVERSE
//! transform to present the content upright. This demo drives a real in-process wayland-client that
//! attaches a NON-SQUARE buffer with four distinctly-colored corner markers, sets
//! `wl_surface.set_buffer_transform` to 90 / 180 / 270, and asserts the composited output shows the
//! content EXACTLY rotated — each corner marker lands in the surface corner the transform dictates, and
//! the presented logical size has width/height swapped for the 90°/270° rotations.
//!
//! The buffer is 80×40 (BUF_W × BUF_H) with corner markers:
//!   RED = buffer top-left, GREEN = buffer top-right, BLUE = buffer bottom-left, YELLOW = buffer
//!   bottom-right. A rotation moves each marker to a predictable surface corner (see the per-transform
//!   expectations below), which we assert pixel-exact and confirm from the written PNG.

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

/// Paint the source buffer: gray fill with a distinct colored marker in each corner.
fn source_buffer() -> Vec<u8> {
    let mut px = solid(BUF_W, BUF_H, GRAY);
    fill_rect(&mut px, BUF_W, BUF_H, 0, 0, M, M, RED); // top-left
    fill_rect(&mut px, BUF_W, BUF_H, BUF_W - M, 0, M, M, GREEN); // top-right
    fill_rect(&mut px, BUF_W, BUF_H, 0, BUF_H - M, M, M, BLUE); // bottom-left
    fill_rect(&mut px, BUF_W, BUF_H, BUF_W - M, BUF_H - M, M, M, YELLOW); // bottom-right
    px
}

/// The four corner colors of a captured frame, sampled INSIDE the marker (2px inset) at each corner.
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

/// Drive one buffer transform on a fresh client + surface, and return the composited frame.
fn run_transform(h: &Harness, tag: &str, transform: Transform) -> CapturedFrame {
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

    // The presented logical size for a 90/270 rotation is swapped (BUF_H × BUF_W); 180 keeps it.
    let (ew, eh) = match transform {
        Transform::_90 | Transform::_270 => (BUF_H, BUF_W),
        _ => (BUF_W, BUF_H),
    };
    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, move |f| {
        f.width == ew && f.height == eh && f.pixel(2, 2).is_some()
    })
    .unwrap_or_else(|| panic!("transformed frame never presented for {tag}"));

    // Keep shell objects alive for the client's lifetime.
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(surface);
    std::mem::forget(buffer);
    frame
}

#[test]
fn buffer_transform_rotate() {
    let h = Harness::start("buffer_transform_rotate");

    // ---- 90° (counter-clockwise) : surface is BUF_H × BUF_W ----
    // buffer TL(RED)->surface BL, TR(GREEN)->TL, BL(BLUE)->BR, BR(YELLOW)->TR.
    let f90 = run_transform(&h, "rot90", Transform::_90);
    assert_eq!(
        (f90.width, f90.height),
        (BUF_H, BUF_W),
        "90 swaps presented dimensions"
    );
    assert_eq!(
        (f90.logical_width, f90.logical_height),
        (BUF_H, BUF_W),
        "90 swaps logical size"
    );
    let (tl, tr, bl, br) = corners(&f90);
    assert_eq!(tl, GREEN, "90: surface top-left = buffer top-right (green)");
    assert_eq!(
        tr, YELLOW,
        "90: surface top-right = buffer bottom-right (yellow)"
    );
    assert_eq!(bl, RED, "90: surface bottom-left = buffer top-left (red)");
    assert_eq!(
        br, BLUE,
        "90: surface bottom-right = buffer bottom-left (blue)"
    );
    save_frame("buffer_transform_rotate-90", &f90);

    // ---- 180° : surface stays BUF_W × BUF_H, content point-reflected ----
    // buffer TL(RED)->surface BR, TR(GREEN)->BL, BL(BLUE)->TR, BR(YELLOW)->TL.
    let f180 = run_transform(&h, "rot180", Transform::_180);
    assert_eq!(
        (f180.width, f180.height),
        (BUF_W, BUF_H),
        "180 keeps presented dimensions"
    );
    let (tl, tr, bl, br) = corners(&f180);
    assert_eq!(
        tl, YELLOW,
        "180: surface top-left = buffer bottom-right (yellow)"
    );
    assert_eq!(
        tr, BLUE,
        "180: surface top-right = buffer bottom-left (blue)"
    );
    assert_eq!(
        bl, GREEN,
        "180: surface bottom-left = buffer top-right (green)"
    );
    assert_eq!(br, RED, "180: surface bottom-right = buffer top-left (red)");
    save_frame("buffer_transform_rotate-180", &f180);

    // ---- 270° (counter-clockwise) : surface is BUF_H × BUF_W ----
    // buffer TL(RED)->surface TR, TR(GREEN)->BR, BL(BLUE)->TL, BR(YELLOW)->BL.
    let f270 = run_transform(&h, "rot270", Transform::_270);
    assert_eq!(
        (f270.width, f270.height),
        (BUF_H, BUF_W),
        "270 swaps presented dimensions"
    );
    let (tl, tr, bl, br) = corners(&f270);
    assert_eq!(
        tl, BLUE,
        "270: surface top-left = buffer bottom-left (blue)"
    );
    assert_eq!(tr, RED, "270: surface top-right = buffer top-left (red)");
    assert_eq!(
        bl, YELLOW,
        "270: surface bottom-left = buffer bottom-right (yellow)"
    );
    assert_eq!(
        br, GREEN,
        "270: surface bottom-right = buffer top-right (green)"
    );
    save_frame("buffer_transform_rotate-270", &f270);

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
                // Declare the buffer transform, then attach the pre-rotated buffer + commit.
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
