//! DEMO — `transform_only_commit`.
//!
//! A commit that changes ONLY `wl_surface.set_buffer_transform` — no new `wl_buffer` attached, no
//! `wl_surface.damage` — must still re-present the retained buffer under the new orientation. The buffer
//! transform is double-buffered surface state; a client is entitled to rotate its already-attached content
//! and see it turn without re-uploading pixels. If the adapter only marked a surface dirty on a fresh
//! attach/damage, the rotation would silently wait for the NEXT buffer (a stale, un-rotated frame lingers).
//!
//! This demo drives a real in-process wayland-client that (1) attaches an 80×40 buffer with four distinct
//! corner markers and confirms it composites UPRIGHT (80×40, RED/GREEN/BLUE/YELLOW corners), then (2)
//! commits ONLY `set_buffer_transform(90)` — no attach, no damage — and asserts the compositor RE-PRESENTS
//! the SAME buffer rotated 90° (now 40×80, corners moved exactly as the rotation dictates). A PNG of each
//! phase is written. Without the self-dirty fix, phase (2) never re-presents and the wait times out.

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
const GRAY: [u8; 4] = [0x30, 0x30, 0x30, 0xFF];

fn source_buffer() -> Vec<u8> {
    let mut px = solid(BUF_W, BUF_H, GRAY);
    fill_rect(&mut px, BUF_W, BUF_H, 0, 0, M, M, RED);
    fill_rect(&mut px, BUF_W, BUF_H, BUF_W - M, 0, M, M, GREEN);
    fill_rect(&mut px, BUF_W, BUF_H, 0, BUF_H - M, M, M, BLUE);
    fill_rect(&mut px, BUF_W, BUF_H, BUF_W - M, BUF_H - M, M, M, YELLOW);
    px
}

/// The four corner colors of a captured frame, sampled 2px inside each corner.
fn corners(f: &CapturedFrame) -> ([u8; 4], [u8; 4], [u8; 4], [u8; 4]) {
    let (w, h) = (f.width, f.height);
    (
        f.pixel(2, 2).unwrap(),
        f.pixel(w - 3, 2).unwrap(),
        f.pixel(2, h - 3).unwrap(),
        f.pixel(w - 3, h - 3).unwrap(),
    )
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
}

#[test]
fn transform_only_commit() {
    let h = Harness::start("transform_only_commit");

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
        "toc",
        BUF_W,
        BUF_H,
        &source_buffer(),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-transform-only-commit".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        configured: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.configured {
        assert!(Instant::now() < deadline, "toplevel never configured");
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch configure");
    }

    // ---- Phase 1: the buffer composites UPRIGHT (transform Normal), 80×40. ----
    let upright = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == BUF_W && f.height == BUF_H && f.pixel(2, 2).is_some()
    })
    .expect("upright frame never presented");
    assert_eq!(
        (upright.width, upright.height),
        (BUF_W, BUF_H),
        "phase 1 is upright dimensions"
    );
    let (tl, tr, bl, br) = corners(&upright);
    assert_eq!(
        (tl, tr, bl, br),
        (RED, GREEN, BLUE, YELLOW),
        "phase 1 corners upright"
    );
    save_frame("transform_only_commit-1-upright", &upright);

    // ---- Phase 2: commit ONLY a buffer transform — NO attach, NO damage. ----
    surface.set_buffer_transform(Transform::_90);
    surface.commit();

    // The SAME buffer must re-present rotated 90° → 40×80 with corners moved per the rotation.
    // (buffer TL(RED)→surface BL, TR(GREEN)→TL, BL(BLUE)→BR, BR(YELLOW)→TR.)
    let rotated = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == BUF_H && f.height == BUF_W && f.pixel_is(2, 2, GREEN)
    })
    .expect("transform-only commit did not re-present (surface never marked dirty)");

    assert_eq!(
        (rotated.width, rotated.height),
        (BUF_H, BUF_W),
        "phase 2 swaps dimensions (90°)"
    );
    assert_eq!(
        (rotated.logical_width, rotated.logical_height),
        (BUF_H, BUF_W),
        "phase 2 swaps logical size"
    );
    let (tl, tr, bl, br) = corners(&rotated);
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
    save_frame("transform_only_commit-2-rotated", &rotated);

    h.shutdown();
    let _ = (toplevel, xdg, buffer);
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
                // Attach the buffer with the DEFAULT (Normal) transform — the rotation comes later.
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
