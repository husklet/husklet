//! DEMO — `damage_region_precision` (precision battery: damage neither over- nor under-reports, exactly).
//!
//! One mapped toplevel drives a sequence of partial-damage commits, and the test asserts, pixel by pixel,
//! that each present carries EXACTLY the changed region and is byte-identical everywhere else:
//!
//!   1. OVERLAPPING + ADJACENT coalesce to the exact union: a commit paints three rects a new color — two
//!      of them sharing an edge (adjacent, no gap, no double-count) and a third overlapping both — and
//!      submits all three as separate `wl_surface.damage` rects in ONE commit. The changed-pixel count is
//!      EXACTLY the union area (overlap counted once, adjacency leaves no seam), everything outside is
//!      byte-identical, and the reported damage origin is the union bounding-box top-left.
//!   2. CLAMPED to surface bounds: a commit damages a rect that OVERHANGS the surface (extends past the
//!      right/bottom edge). Only the in-bounds portion changes; the compositor neither reads nor writes
//!      out-of-bounds, presents cleanly, and everything outside the in-bounds sub-rect is byte-identical.
//!   3. ZERO-damage re-present: a commit of byte-identical pixels with NO damage re-presents a frame
//!      byte-identical to the previous one — a re-present neither corrupts nor drops content.
//!
//! Like `demo_damage_coalescing`, the headless presenter captures the FULL deposited buffer (not a
//! damage-clipped upload), so the per-pixel evidence is the buffer content while the coalesced/clamped
//! damage evidence is the single present + the reported bounding-box origin.

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

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x22, 0x22, 0x28, 0xFF]; // dark slate
const NEW: [u8; 4] = [0xF0, 0xA0, 0x20, 0xFF]; // amber
const NEW2: [u8; 4] = [0x20, 0xC0, 0x80, 0xFF]; // teal

// Three damage rects (x, y, w, h): R1 and R2 are ADJACENT (R2 begins exactly where R1 ends on x, sharing
// the vertical edge x=90 — no overlap, no gap), R3 OVERLAPS both. The union is a non-rectangular blob so
// overlap is double-counted iff coalescing is wrong and adjacency leaves a seam iff it under-reports.
const RECTS: [(i32, i32, i32, i32); 3] = [
    (50, 40, 40, 30), // R1  → x in [50,90)
    (90, 40, 40, 30), // R2  → x in [90,130); shares edge x=90 with R1
    (75, 25, 20, 70), // R3  → overlaps R1 and R2
];

// An OVERHANGING damage rect: its in-bounds portion is [150,200)×[120,150); it extends to x=250, y=200,
// well past the W=200,H=150 surface. Only the in-bounds part may change.
const OVER: (i32, i32, i32, i32) = (150, 120, 100, 80);
const OVER_INB: (i32, i32, i32, i32) = (150, 120, W - 150, H - 120); // clamped to bounds

fn in_rects(x: i32, y: i32) -> bool {
    RECTS
        .iter()
        .any(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
}
fn in_over_inbounds(x: i32, y: i32) -> bool {
    let (rx, ry, rw, rh) = OVER_INB;
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

/// The exact pixel count of the union of `RECTS` (overlap once) and its bounding box.
fn union_area_and_bbox() -> (u64, (i32, i32, i32, i32)) {
    let mut area = 0u64;
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in 0..H {
        for x in 0..W {
            if in_rects(x, y) {
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
fn damage_region_precision() {
    let h = Harness::start("damage_region_precision");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // buf1: solid BASE.
    // buf2: BASE with the three coalescing rects painted NEW.
    // buf3: buf2 with the CLAMPED overhang region additionally painted NEW2 (fill_rect clips to bounds).
    // buf4: identical to buf3 (for the zero-damage re-present).
    let px1 = solid(W, H, BASE);
    let mut px2 = solid(W, H, BASE);
    for &(rx, ry, rw, rh) in &RECTS {
        fill_rect(&mut px2, W, H, rx, ry, rw, rh, NEW);
    }
    let mut px3 = px2.clone();
    let (orx, ory, orw, orh) = OVER;
    fill_rect(&mut px3, W, H, orx, ory, orw, orh, NEW2); // clipped to in-bounds by fill_rect
    let px4 = px3.clone();

    let buf1 = make_buffer(&shm, &qh, &h.runtime_dir, "b1", W, H, &px1);
    let buf2 = make_buffer(&shm, &qh, &h.runtime_dir, "b2", W, H, &px2);
    let buf3 = make_buffer(&shm, &qh, &h.runtime_dir, "b3", W, H, &px3);
    let buf4 = make_buffer(&shm, &qh, &h.runtime_dir, "b4", W, H, &px4);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-damage-precision".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buf1: buf1.clone(),
        drawn: false,
        frame_done: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // ---- frame 1: BASE ----
    let frame1 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, BASE)
    })
    .expect("initial BASE frame never composited");
    assert_eq!(frame1.serial, 1, "map is present #1");
    assert_eq!(
        h.captures.lock().unwrap().len(),
        1,
        "one present after the map"
    );

    // ---- frame 2: three coalescing (overlap + adjacent) damage rects in ONE commit ----
    app.surface.attach(Some(&buf2), 0, 0);
    for &(rx, ry, rw, rh) in &RECTS {
        app.surface.damage(rx, ry, rw, rh);
    }
    let _cb2: WlCallback = app.surface.frame(&qh, ());
    app.surface.commit();
    let frame2 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > frame1.serial
    })
    .expect("coalesced-damage commit never presented");
    assert_eq!(frame2.serial, 2, "coalesced damage presents as present #2");
    std::thread::sleep(Duration::from_millis(60));
    let _ = queue.roundtrip(&mut app);
    assert_eq!(
        h.captures.lock().unwrap().len(),
        2,
        "no double-present: exactly two frames total"
    );

    let (union_area, (bx, by, bw, bh)) = union_area_and_bbox();
    assert_eq!(
        (frame2.x, frame2.y),
        (bx, by),
        "reported damage origin == union bounding-box top-left"
    );
    // Adjacency sanity: the union spans BOTH adjacent rects horizontally (no seam collapse), and R1|R2
    // alone (adjacent, no overlap) contribute their full combined width.
    assert!(
        bw >= 80,
        "union bbox width {bw} spans both adjacent rects (>= 80)"
    );

    let mut changed = 0u64;
    for y in 0..H {
        for x in 0..W {
            let (p1, p2) = (frame1.pixel(x, y).unwrap(), frame2.pixel(x, y).unwrap());
            if in_rects(x, y) {
                assert_eq!(p2, NEW, "pixel ({x},{y}) in the union is NEW");
                assert_ne!(p2, p1, "pixel ({x},{y}) in the union actually changed");
                changed += 1;
            } else {
                assert_eq!(
                    p2, p1,
                    "pixel ({x},{y}) outside the union is byte-identical"
                );
                assert_eq!(p2, BASE, "pixel ({x},{y}) outside the union is still BASE");
            }
        }
    }
    assert_eq!(
        changed, union_area,
        "changed-pixel count equals the exact union area (overlap once, no seam)"
    );

    // ---- frame 3: an OVERHANGING damage rect — only its in-bounds portion may change ----
    app.surface.attach(Some(&buf3), 0, 0);
    let (orx, ory, orw, orh) = OVER;
    app.surface.damage(orx, ory, orw, orh); // overhangs W×H
    let _cb3: WlCallback = app.surface.frame(&qh, ());
    app.surface.commit();
    let frame3 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > frame2.serial
    })
    .expect("overhang-damage commit never presented");
    assert_eq!(frame3.serial, 3, "overhang damage presents as present #3");
    // Reported origin is the CLAMPED in-bounds top-left (the overhang did not push the origin off-surface).
    assert_eq!(
        (frame3.x, frame3.y),
        (OVER_INB.0, OVER_INB.1),
        "overhang damage origin clamped to surface"
    );

    let mut over_changed = 0u64;
    for y in 0..H {
        for x in 0..W {
            let (p2, p3) = (frame2.pixel(x, y).unwrap(), frame3.pixel(x, y).unwrap());
            if in_over_inbounds(x, y) {
                assert_eq!(
                    p3, NEW2,
                    "pixel ({x},{y}) in the in-bounds overhang is NEW2"
                );
                assert_ne!(p3, p2, "pixel ({x},{y}) in the in-bounds overhang changed");
                over_changed += 1;
            } else {
                assert_eq!(
                    p3, p2,
                    "pixel ({x},{y}) outside the in-bounds overhang is byte-identical to frame2"
                );
            }
        }
    }
    assert_eq!(
        over_changed,
        (OVER_INB.2 * OVER_INB.3) as u64,
        "exactly the CLAMPED in-bounds sub-rect changed (overhang did not affect out-of-bounds)"
    );

    // ---- frame 4: identical pixels, NO damage → re-present byte-identical to frame 3 ----
    app.surface.attach(Some(&buf4), 0, 0);
    // deliberately NO surface.damage(...)
    let _cb4: WlCallback = app.surface.frame(&qh, ());
    app.surface.commit();
    let frame4 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > frame3.serial && f.pixel_is(1, 1, BASE)
    })
    .expect("zero-damage re-present never captured");
    assert_eq!(
        frame4.rgba, frame3.rgba,
        "zero-damage re-present is byte-identical to the damaged frame"
    );

    save_frame("damage_region_precision-2_union", &frame2);
    save_frame("damage_region_precision-3_overhang", &frame3);

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
        qh: &QueueHandle<Self>,
    ) {
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
    fn event(
        app: &mut Self,
        _: &WlCallback,
        e: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    XdgToplevel
);
