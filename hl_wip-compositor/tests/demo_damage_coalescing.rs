//! DEMO (batch-3) — `damage_coalescing` (several overlapping damage rects → one present, exact union).
//!
//! A mapped toplevel commits a second buffer that paints THREE overlapping rectangles a new color, and
//! submits all three as SEPARATE `wl_surface.damage` rects in the SAME commit. The test asserts:
//!
//!   * the coalesced damage presents in EXACTLY ONE new frame (no double-present, no per-rect present);
//!   * the compositor coalesces the three rects into their bounding box: the presented frame reports its
//!     changed region's top-left at the exact union bounding-box origin (`is_tree_dirty` +
//!     `DamageRegion::bounding_box`);
//!   * pixel by pixel, the changed set between the two frames is EXACTLY the union of the three rects —
//!     overlap counted once, no pixel missed, none spuriously changed (the exact union changed-pixel
//!     count).
//!
//! Note the headless presenter captures the FULL deposited buffer (not a damage-clipped upload), so the
//! per-pixel evidence is the buffer content while the coalesced-damage evidence is the single present +
//! the reported bounding-box origin.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const BASE: [u8; 4] = [0x22, 0x22, 0x28, 0xFF]; // dark slate
const NEW: [u8; 4] = [0xF0, 0xA0, 0x20, 0xFF]; // amber

// Three OVERLAPPING damage rects (x, y, w, h). Chosen asymmetric + mutually overlapping so the union is a
// non-rectangular blob, the bounding-box origin is unambiguous (30,15), and overlap is double-counted iff
// the compositor mis-coalesces.
const RECTS: [(i32, i32, i32, i32); 3] = [
    (30, 20, 40, 30), // R1
    (55, 35, 40, 30), // R2 — overlaps R1
    (45, 15, 20, 50), // R3 — overlaps both
];

fn in_any_rect(x: i32, y: i32) -> bool {
    RECTS.iter().any(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
}

/// The exact pixel count of the union of the three rects (overlap counted once) and its bounding box.
fn union_area_and_bbox() -> (u64, (i32, i32, i32, i32)) {
    let mut area = 0u64;
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in 0..H {
        for x in 0..W {
            if in_any_rect(x, y) {
                area += 1;
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x + 1);
                maxy = maxy.max(y + 1);
            }
        }
    }
    (area, (minx, miny, maxx - minx, maxy - miny))
}

struct App {
    surface: WlSurface,
    buf1: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn damage_coalescing() {
    let h = Harness::start("damage_coalescing");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // buf1: solid BASE. buf2: BASE with the three overlapping rects painted NEW.
    let px1 = solid(W, H, BASE);
    let mut px2 = solid(W, H, BASE);
    for &(rx, ry, rw, rh) in &RECTS {
        fill_rect(&mut px2, W, H, rx, ry, rw, rh, NEW);
    }
    let buf1 = make_buffer(&shm, &qh, &h.runtime_dir, "b1", W, H, &px1);
    let buf2 = make_buffer(&shm, &qh, &h.runtime_dir, "b2", W, H, &px2);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-coalesce".into());
    surface.commit();

    let mut app = App { surface: surface.clone(), buf1: buf1.clone(), drawn: false, frame_done: false };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // ---- frame 1: BASE ----
    let frame1 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, BASE))
        .expect("initial BASE frame never composited");
    assert_eq!(frame1.serial, 1, "map is present #1");
    assert_eq!(h.captures.lock().unwrap().len(), 1, "one present after the map");

    // ---- frame 2: attach buf2, submit the THREE overlapping damage rects in ONE commit ----
    app.surface.attach(Some(&buf2), 0, 0);
    for &(rx, ry, rw, rh) in &RECTS {
        app.surface.damage(rx, ry, rw, rh);
    }
    let _cb: WlCallback = app.surface.frame(&qh, ());
    app.surface.commit();

    let frame2 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.serial > frame1.serial)
        .expect("coalesced-damage commit never presented");

    // Single present: exactly ONE new frame for the whole coalesced-damage commit.
    assert_eq!(frame2.serial, 2, "coalesced damage presents as present #2");
    // Give the serve loop a moment; confirm NO extra (double) present appeared.
    std::thread::sleep(Duration::from_millis(60));
    let _ = queue.roundtrip(&mut app);
    assert_eq!(h.captures.lock().unwrap().len(), 2, "no double-present: exactly two frames total");

    // Coalesced bounding box: the reported changed-region origin is the union bbox top-left.
    let (union_area, (bx, by, bw, bh)) = union_area_and_bbox();
    assert_eq!((frame2.x, frame2.y), (bx, by), "reported damage origin == union bounding-box top-left");

    // Exact per-pixel union: changed IFF inside any rect; overlap counted once; nothing else touched.
    let mut changed = 0u64;
    for y in 0..H {
        for x in 0..W {
            let (p1, p2) = (frame1.pixel(x, y).unwrap(), frame2.pixel(x, y).unwrap());
            if in_any_rect(x, y) {
                assert_eq!(p2, NEW, "pixel ({x},{y}) in the union is NEW");
                assert_ne!(p2, p1, "pixel ({x},{y}) in the union actually changed");
                changed += 1;
            } else {
                assert_eq!(p2, p1, "pixel ({x},{y}) outside the union is byte-identical");
                assert_eq!(p2, BASE, "pixel ({x},{y}) outside the union is still BASE");
            }
        }
    }
    assert_eq!(changed, union_area, "changed-pixel count equals the exact union area (overlap counted once)");
    // Sanity: the union bbox strictly exceeds any single rect (the rects really do span it).
    assert!(bw > 40 && bh > 30, "union bbox {bw}x{bh} spans multiple rects");

    save_frame("damage_coalescing-1_base", &frame1);
    save_frame("damage_coalescing-2_union", &frame2);

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
                app.surface.attach(Some(&app.buf1), 0, 0);
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel);
