//! DEMO (interactive-latency battery) — `input_burst_no_backlog` (a burst of N rapid inputs is ALL
//! delivered and the last input's render presents promptly — the queue never backs up / grows unbounded).
//!
//! A mapped toplevel with a `wl_pointer` + `wl_keyboard`. The test injects a burst of N=60 inputs (59 key
//! events + a final pointer motion) as fast as the channel accepts them, then asserts:
//!
//!   * EVERY one of the 60 inputs was delivered to the client (59 `wl_keyboard.key` + 1 pointer enter) —
//!     nothing dropped, the delivered count == N exactly;
//!   * the LAST input's effect (a marker painted at the pointer position) reaches the screen within a
//!     BOUNDED number of present cycles and a generous wall-clock ceiling — the burst did not build a
//!     backlog that delays the final render.
//!
//! This is the "rapid input does not queue up" proof: a flood of events is drained promptly and the
//! resulting frame is not stuck behind them.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard}, wl_pointer::{self, WlPointer}, wl_registry::WlRegistry,
    wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x26, 0x22, 0x20, 0xFF];
const MARKER: [u8; 4] = [0xF0, 0x60, 0xC0, 0xFF];
const MK: i32 = 12;
const PX: i32 = 130;
const PY: i32 = 90;
const KEY_A: u32 = 30;
const N_KEYS: u32 = 59; // + 1 final pointer motion == 60 total inputs
const LATENCY_CEILING: Duration = Duration::from_millis(2000);

struct App {
    surface: WlSurface,
    marker_buffer: WlBuffer,
    base_buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    marker_drawn: bool,
    keys: u32,
    enters: u32,
}

#[test]
fn input_burst_no_backlog() {
    let h = Harness::start("input_burst_no_backlog");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let mut marker_px = solid(W, H, BASE);
    fill_rect(&mut marker_px, W, H, PX - MK / 2, PY - MK / 2, MK, MK, MARKER);
    let marker_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "marker", W, H, &marker_px);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-burst".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(), marker_buffer: marker_buffer.clone(), base_buffer: base_buffer.clone(),
        drawn: false, frame_done: false, marker_drawn: false, keys: 0, enters: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let base_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, BASE))
        .expect("base frame never composited");
    let baseline = base_frame.serial;

    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _keyboard: WlKeyboard = seat.get_keyboard(&qh, ());
    h.input_tx.send(InputCommand::FocusTopmostKeyboard).unwrap();
    let _ = queue.roundtrip(&mut app);

    // ---- the burst: N_KEYS key events, then a final pointer motion (the 60th input) ----
    let t0 = Instant::now();
    for i in 0..N_KEYS {
        h.input_tx.send(InputCommand::Key { keycode: KEY_A, pressed: i % 2 == 0 }).unwrap();
    }
    h.input_tx.send(InputCommand::PointerMotion { x: PX as f64, y: PY as f64 }).unwrap();

    // Wait until all inputs are delivered AND the last input's marker presents.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut marker_frame = None;
    let mut t1 = None;
    loop {
        let _ = queue.roundtrip(&mut app);
        if marker_frame.is_none() {
            if let Some(f) = h.captures.lock().unwrap().iter().rev().find(|f| f.pixel_is(PX, PY, MARKER)).cloned() {
                t1 = Some(Instant::now());
                marker_frame = Some(f);
            }
        }
        if app.keys >= N_KEYS && app.enters >= 1 && marker_frame.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "burst not drained: keys={} enters={} marker={}", app.keys, app.enters, marker_frame.is_some()
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // ---- (a) every input delivered, exactly N (no drops, no backlog growth) ----
    assert_eq!(app.keys, N_KEYS, "all {N_KEYS} key events delivered");
    assert_eq!(app.enters, 1, "the final pointer input delivered");
    let delivered = app.keys + app.enters;
    assert_eq!(delivered, 60, "exactly 60 inputs delivered (delivered count == N)");

    // ---- (b) the last input's render presented within a bounded lag ----
    let mf = marker_frame.unwrap();
    let elapsed = t1.unwrap().duration_since(t0);
    let cycles = mf.serial - baseline;
    assert!(cycles >= 1 && cycles <= 3, "final render bounded to a few cycles (serial delta {cycles})");
    assert!(elapsed < LATENCY_CEILING, "burst→final-present {elapsed:?} exceeded ceiling {LATENCY_CEILING:?}");
    assert_eq!(mf.pixel(PX, PY).unwrap(), MARKER, "final marker at the injected pointer position");

    eprintln!("input_burst: delivered={delivered} final_cycles={cycles} wall={elapsed:?}");
    save_frame("input_burst_no_backlog-marker", &mf);
    h.shutdown();
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
                app.surface.attach(Some(&app.base_buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlPointer, ()> for App {
    fn event(app: &mut Self, _: &WlPointer, e: <WlPointer as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_pointer::Event::Enter { .. } = e {
            app.enters += 1;
            if !app.marker_drawn {
                app.surface.attach(Some(&app.marker_buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.marker_drawn = true;
            }
        }
    }
}
impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &WlKeyboard, e: <WlKeyboard as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_keyboard::Event::Key { .. } = e { app.keys += 1; }
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, XdgToplevel);
