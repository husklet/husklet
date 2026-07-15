//! DEMO 3 — `subsurface_positions_and_zorder`.
//!
//! A toplevel plus two OVERLAPPING desynchronized `wl_subsurface`s, each at a `set_position` offset in
//! its own color. We assert each composites at EXACTLY its `parent + set_position`, and that the
//! composite z-order tracks `place_above`/`place_below`: because the headless presenter captures layers
//! (not a blended framebuffer), the z-order evidence is the PRESENT ORDER (serial) within a compose
//! cycle — which is exactly the stacking a real backend blends. A composited PNG is written per stacking
//! (before/after the reorder) so the overlap region visibly flips color.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::CapturedFrame;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const TL_W: i32 = 300;
const TL_H: i32 = 200;
const TL: [u8; 4] = [0x20, 0x20, 0xC0, 0xFF]; // blue

const A_W: i32 = 50;
const A_H: i32 = 24;
const A_COL: [u8; 4] = [0x10, 0xD0, 0x20, 0xFF]; // green
const A_POS: (i32, i32) = (48, 66);

const B_W: i32 = 44;
const B_H: i32 = 30;
const B_COL: [u8; 4] = [0xD0, 0x10, 0xC0, 0xFF]; // magenta
const B_POS: (i32, i32) = (60, 78);

struct App {
    tl_surface: WlSurface,
    tl_buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
}

fn is_a(f: &CapturedFrame) -> bool {
    f.width == A_W && f.height == A_H && f.pixel_is(A_W / 2, A_H / 2, A_COL)
}
fn is_b(f: &CapturedFrame) -> bool {
    f.width == B_W && f.height == B_H && f.pixel_is(B_W / 2, B_H / 2, B_COL)
}
fn newest_serial(caps: &std::sync::Arc<std::sync::Mutex<Vec<CapturedFrame>>>, pred: impl Fn(&CapturedFrame) -> bool) -> Option<u64> {
    caps.lock().unwrap().iter().filter(|f| pred(f)).map(|f| f.serial).max()
}

#[test]
fn subsurface_positions_and_zorder() {
    let h = Harness::start("subsurface_zorder");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let subcompositor: WlSubcompositor = globals.bind(&qh, 1..=1, ()).expect("wl_subcompositor");

    let tl_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "tl", TL_W, TL_H, &solid(TL_W, TL_H, TL));
    let a_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "a", A_W, A_H, &solid(A_W, A_H, A_COL));
    let b_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "b", B_W, B_H, &solid(B_W, B_H, B_COL));

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-subsurface".into());
    tl_surface.commit();

    let mut app = App { tl_surface: tl_surface.clone(), tl_buffer: tl_buffer.clone(), tl_drawn: false, tl_frame_done: false };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == TL_W && f.pixel_is(1, 1, TL)).is_some(),
        "toplevel never composited",
    );

    // ---- subsurface A (green) at A_POS ----
    let a_surface = compositor.create_surface(&qh, ());
    let _sub_a: WlSubsurface = subcompositor.get_subsurface(&a_surface, &tl_surface, &qh, ());
    _sub_a.set_position(A_POS.0, A_POS.1);
    _sub_a.set_desync();
    a_surface.attach(Some(&a_buffer), 0, 0);
    a_surface.damage(0, 0, A_W, A_H);
    a_surface.commit();
    tl_surface.commit();

    let a_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| is_a(f) && (f.x, f.y) == A_POS)
        .expect("subsurface A never composited at parent + set_position");
    assert_eq!((a_frame.x, a_frame.y), A_POS, "subsurface A exact placement");
    assert_eq!(a_frame.pixel(A_W / 2, A_H / 2).unwrap(), A_COL, "subsurface A color");

    // ---- subsurface B (magenta) at B_POS, created after A -> B stacks ABOVE A by default ----
    let b_surface = compositor.create_surface(&qh, ());
    let sub_b: WlSubsurface = subcompositor.get_subsurface(&b_surface, &tl_surface, &qh, ());
    sub_b.set_position(B_POS.0, B_POS.1);
    sub_b.set_desync();
    b_surface.attach(Some(&b_buffer), 0, 0);
    b_surface.damage(0, 0, B_W, B_H);
    b_surface.commit();
    tl_surface.commit();

    let b_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| is_b(f) && (f.x, f.y) == B_POS)
        .expect("subsurface B never composited at parent + set_position");
    assert_eq!((b_frame.x, b_frame.y), B_POS, "subsurface B exact placement");
    assert_eq!(b_frame.pixel(B_W / 2, B_H / 2).unwrap(), B_COL, "subsurface B color");
    assert_ne!(A_COL, B_COL);

    // Default z-order: B was created after A, so B composites ABOVE A (later present serial).
    let a_serial_before = newest_serial(&h.captures, is_a).expect("A serial");
    let b_serial_before = newest_serial(&h.captures, is_b).expect("B serial");
    assert!(b_serial_before > a_serial_before, "B (created later) composites above A: b={b_serial_before} a={a_serial_before}");
    // Composited PNG: B on top (overlap region is magenta).
    let a0 = h.captures.lock().unwrap().iter().rev().find(|f| is_a(f)).cloned().unwrap();
    let b0 = h.captures.lock().unwrap().iter().rev().find(|f| is_b(f)).cloned().unwrap();
    save_composited("subsurface_zorder-frame0-Btop", TL_W, TL_H, TL, &[(&a0, A_POS.0, A_POS.1), (&b0, B_POS.0, B_POS.1)]);

    // ---- reorder: place A ABOVE B, then force a re-present ----
    _sub_a.place_above(&b_surface);
    tl_surface.attach(Some(&tl_buffer), 0, 0);
    tl_surface.damage(0, 0, TL_W, TL_H);
    tl_surface.commit();

    // After the reorder cycle A must present ABOVE B (A's newest serial exceeds B's newest serial).
    let reordered = pump_while(&mut queue, &mut app, 5, |_| {
        match (newest_serial(&h.captures, is_a), newest_serial(&h.captures, is_b)) {
            (Some(a), Some(b)) => a > b && a > a_serial_before,
            _ => false,
        }
    });
    let a_serial_after = newest_serial(&h.captures, is_a).unwrap();
    let b_serial_after = newest_serial(&h.captures, is_b).unwrap();
    assert!(reordered, "after place_above, A never composited above B: a={a_serial_after} b={b_serial_after}");
    assert!(a_serial_after > b_serial_after, "z-order flipped: A now above B");

    // Composited PNG: A on top (overlap region is green).
    let a1 = h.captures.lock().unwrap().iter().rev().find(|f| is_a(f)).cloned().unwrap();
    let b1 = h.captures.lock().unwrap().iter().rev().find(|f| is_b(f)).cloned().unwrap();
    save_composited("subsurface_zorder-frame1-Atop", TL_W, TL_H, TL, &[(&b1, B_POS.0, B_POS.1), (&a1, A_POS.0, A_POS.1)]);

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
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.tl_drawn {
                app.tl_surface.attach(Some(&app.tl_buffer), 0, 0);
                app.tl_surface.damage(0, 0, TL_W, TL_H);
                let _cb: WlCallback = app.tl_surface.frame(qh, ());
                app.tl_surface.commit();
                app.tl_drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for App {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.tl_frame_done = true;
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSubcompositor, WlSubsurface, XdgToplevel);
