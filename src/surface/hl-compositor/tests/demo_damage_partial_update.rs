//! DEMO 7 — `damage_partial_update` (damage tracking neither over- nor under-reports).
//!
//! A single client maps a toplevel with a solid BASE buffer, then commits a SECOND buffer that differs
//! from the first ONLY inside a sub-rectangle, damaging ONLY that sub-rect. The test asserts, pixel by
//! pixel, that the newly composited frame differs from the previous one at EXACTLY the sub-rect and is
//! byte-identical everywhere else — the whole wl → scene → present path carried the change faithfully
//! (no smear from an over-reported region, no stale pixels from an under-reported one). It then commits a
//! THIRD buffer with identical pixels and NO damage and asserts the re-presented frame is byte-identical
//! to the second — a zero-damage re-present neither corrupts nor drops content.

mod client_harness;
use client_harness::*;

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
const BASE: [u8; 4] = [0x33, 0x33, 0x33, 0xFF]; // dark gray
const NEW: [u8; 4] = [0xF0, 0x90, 0x10, 0xFF]; // orange
                                               // The damaged sub-rectangle: asymmetric + off-center so an axis swap or origin drift is caught.
const RX: i32 = 100;
const RY: i32 = 20;
const RW: i32 = 40;
const RH: i32 = 30;

fn in_sub(x: i32, y: i32) -> bool {
    x >= RX && x < RX + RW && y >= RY && y < RY + RH
}

struct App {
    surface: WlSurface,
    buf1: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn damage_partial_update() {
    let h = Harness::start("damage_partial");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // buf1: solid BASE. buf2: BASE everywhere EXCEPT the sub-rect, which is NEW. buf3: identical to buf2.
    let px1 = solid(W, H, BASE);
    let mut px2 = solid(W, H, BASE);
    fill_rect(&mut px2, W, H, RX, RY, RW, RH, NEW);
    let px3 = px2.clone();
    let buf1 = make_buffer(&shm, &qh, &h.runtime_dir, "b1", W, H, &px1);
    let buf2 = make_buffer(&shm, &qh, &h.runtime_dir, "b2", W, H, &px2);
    let buf3 = make_buffer(&shm, &qh, &h.runtime_dir, "b3", W, H, &px3);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-damage".into());
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

    // ---- frame 1: the initial BASE buffer ----
    let frame1 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, BASE)
    })
    .expect("initial BASE frame never composited");
    assert_eq!(
        frame1.pixel(RX + 1, RY + 1).unwrap(),
        BASE,
        "frame1 is BASE inside the (not-yet-damaged) sub-rect"
    );

    // ---- frame 2: commit buf2, damaging ONLY the sub-rect ----
    surface.attach(Some(&buf2), 0, 0);
    surface.damage(RX, RY, RW, RH);
    let _cb: WlCallback = surface.frame(&qh, ());
    surface.commit();
    let frame2 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(RX + RW / 2, RY + RH / 2, NEW)
    })
    .expect("damaged sub-rect never re-composited to NEW");

    // Exact-diff assertion: frame2 vs frame1 differ IFF the pixel is inside the damaged sub-rect.
    let (mut changed_inside, mut identical_outside) = (0u64, 0u64);
    for y in 0..H {
        for x in 0..W {
            let (p1, p2) = (frame1.pixel(x, y).unwrap(), frame2.pixel(x, y).unwrap());
            if in_sub(x, y) {
                assert_eq!(p2, NEW, "pixel ({x},{y}) inside sub-rect is NEW");
                assert_ne!(p2, p1, "pixel ({x},{y}) inside sub-rect actually changed");
                changed_inside += 1;
            } else {
                assert_eq!(
                    p2, p1,
                    "pixel ({x},{y}) OUTSIDE sub-rect is byte-identical to frame1"
                );
                assert_eq!(p2, BASE, "pixel ({x},{y}) outside sub-rect is still BASE");
                identical_outside += 1;
            }
        }
    }
    assert_eq!(
        changed_inside,
        (RW * RH) as u64,
        "exactly the sub-rect changed"
    );
    assert_eq!(
        identical_outside,
        (W * H - RW * RH) as u64,
        "everything else is unchanged"
    );

    // ---- frame 3: identical pixels, NO damage → re-present must be byte-identical to frame 2 ----
    surface.attach(Some(&buf3), 0, 0);
    // deliberately NO surface.damage(...) call
    let _cb3: WlCallback = surface.frame(&qh, ());
    surface.commit();
    let frame3 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > frame2.serial && f.pixel_is(1, 1, BASE)
    })
    .expect("zero-damage re-present never captured");
    assert_eq!(
        frame3.rgba, frame2.rgba,
        "zero-damage re-present is byte-identical to the damaged frame"
    );

    save_frame("damage_partial-frame1_base", &frame1);
    save_frame("damage_partial-frame2_damaged", &frame2);

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
