//! DEMO (batch-input) — `pointer_gestures` (real `zwp_pointer_gestures_v1` trackpad pinch + swipe:
//! begin/update/end with exact scale/rotation/dx/dy + finger count).
//!
//! A mapped toplevel binds `zwp_pointer_gestures_v1` and obtains a swipe + a pinch gesture object for its
//! `wl_pointer`. With the pointer positioned over the surface, the host seam drives a three-finger SWIPE
//! (begin/update/end) and a two-finger PINCH (begin/update/end). The test asserts the client receives, in
//! exact order and with exact values:
//!
//!   * `swipe.begin(fingers=3)` on our surface, `swipe.update(dx, dy)`, `swipe.end(cancelled=false)`;
//!   * `pinch.begin(fingers=2)` on our surface, `pinch.update(dx, dy, scale, rotation)`,
//!     `pinch.end(cancelled=false)`.
//!
//! This proves the gesture adapter delivers genuine multi-finger touchpad gestures with per-field fidelity
//! (finger count, center delta, absolute scale, rotation), targeted at the pointer-focused surface.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_pointer::WlPointer, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_pinch_v1::{self, ZwpPointerGesturePinchV1},
    zwp_pointer_gesture_swipe_v1::{self, ZwpPointerGestureSwipeV1},
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x22, 0x18, 0x28, 0xFF];

/// Recorded gesture event, exact values. Coords/scale/rotation are exact in wl_fixed for the chosen inputs.
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    SwipeBegin { fingers: u32, on_surface: bool },
    SwipeUpdate { dx: i64, dy: i64 },
    SwipeEnd { cancelled: bool },
    PinchBegin { fingers: u32, on_surface: bool },
    PinchUpdate { dx: i64, dy: i64, scale_x1000: i64, rot_x1000: i64 },
    PinchEnd { cancelled: bool },
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    entered: bool,
    events: Vec<Ev>,
}

#[test]
fn pointer_gestures() {
    let h = Harness::start("pointer_gestures");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    let gestures: ZwpPointerGesturesV1 = globals.bind(&qh, 1..=3, ()).expect("zwp_pointer_gestures_v1");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-gestures".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(), buffer: buffer.clone(), drawn: false, frame_done: false,
        entered: false, events: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, BASE))
        .expect("base frame never composited");

    // The pointer + its gesture objects. Gestures target the pointer's focused surface.
    let pointer: WlPointer = seat.get_pointer(&qh, ());
    let _swipe: ZwpPointerGestureSwipeV1 = gestures.get_swipe_gesture(&pointer, &qh, ());
    let _pinch: ZwpPointerGesturePinchV1 = gestures.get_pinch_gesture(&pointer, &qh, ());
    let _ = queue.roundtrip(&mut app);

    // Position the pointer over the surface so it holds pointer focus (gestures need a focused surface).
    h.input_tx.send(InputCommand::PointerMotion { x: 100.0, y: 75.0 }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.entered {
        let _ = queue.roundtrip(&mut app);
        assert!(Instant::now() < deadline, "pointer never entered surface");
        std::thread::sleep(Duration::from_millis(5));
    }

    // ---- three-finger swipe ----
    h.input_tx.send(InputCommand::GestureSwipeBegin { fingers: 3 }).unwrap();
    h.input_tx.send(InputCommand::GestureSwipeUpdate { dx: 10.0, dy: -5.0 }).unwrap();
    h.input_tx.send(InputCommand::GestureSwipeEnd { cancelled: false }).unwrap();
    // ---- two-finger pinch (zoom 1.5x, rotate 15°) ----
    h.input_tx.send(InputCommand::GesturePinchBegin { fingers: 2 }).unwrap();
    h.input_tx.send(InputCommand::GesturePinchUpdate { dx: 4.0, dy: 2.0, scale: 1.5, rotation: 15.0 }).unwrap();
    h.input_tx.send(InputCommand::GesturePinchEnd { cancelled: false }).unwrap();

    let expected = vec![
        Ev::SwipeBegin { fingers: 3, on_surface: true },
        Ev::SwipeUpdate { dx: 10_000, dy: -5_000 },
        Ev::SwipeEnd { cancelled: false },
        Ev::PinchBegin { fingers: 2, on_surface: true },
        Ev::PinchUpdate { dx: 4_000, dy: 2_000, scale_x1000: 1_500, rot_x1000: 15_000 },
        Ev::PinchEnd { cancelled: false },
    ];
    let want = expected.len();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.events.len() < want {
        let _ = queue.roundtrip(&mut app);
        assert!(Instant::now() < deadline, "gesture events incomplete: {:?}", app.events);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.events, expected, "exact swipe + pinch gestures with fingers/delta/scale/rotation");

    h.shutdown();
}

/// Scale an f64 wl_fixed-round-tripped value to fixed-point milliunits for exact integer comparison.
fn m(v: f64) -> i64 {
    (v * 1000.0).round() as i64
}

// ---------- dispatch plumbing ----------
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
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlPointer, ()> for App {
    fn event(app: &mut Self, _: &WlPointer, e: <WlPointer as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_pointer::Event::Enter { .. } = e { app.entered = true; }
    }
}
impl Dispatch<ZwpPointerGestureSwipeV1, ()> for App {
    fn event(app: &mut Self, _: &ZwpPointerGestureSwipeV1, e: <ZwpPointerGestureSwipeV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwp_pointer_gesture_swipe_v1::Event;
        match e {
            Event::Begin { fingers, surface, .. } => {
                app.events.push(Ev::SwipeBegin { fingers, on_surface: surface == app.surface });
            }
            Event::Update { dx, dy, .. } => app.events.push(Ev::SwipeUpdate { dx: m(dx), dy: m(dy) }),
            Event::End { cancelled, .. } => app.events.push(Ev::SwipeEnd { cancelled: cancelled != 0 }),
            _ => {}
        }
    }
}
impl Dispatch<ZwpPointerGesturePinchV1, ()> for App {
    fn event(app: &mut Self, _: &ZwpPointerGesturePinchV1, e: <ZwpPointerGesturePinchV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwp_pointer_gesture_pinch_v1::Event;
        match e {
            Event::Begin { fingers, surface, .. } => {
                app.events.push(Ev::PinchBegin { fingers, on_surface: surface == app.surface });
            }
            Event::Update { dx, dy, scale, rotation, .. } => {
                app.events.push(Ev::PinchUpdate { dx: m(dx), dy: m(dy), scale_x1000: m(scale), rot_x1000: m(rotation) });
            }
            Event::End { cancelled, .. } => app.events.push(Ev::PinchEnd { cancelled: cancelled != 0 }),
            _ => {}
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, XdgToplevel, ZwpPointerGesturesV1);
