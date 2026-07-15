//! DEMO (interactive-latency battery) — `input_to_present_latency` (an input drives a render that reaches
//! the screen PROMPTLY, bounded to a couple present cycles + a generous wall-clock ceiling).
//!
//! A mapped toplevel with a `wl_pointer`. The test records the baseline present serial, then — timing with
//! `Instant` — injects a pointer motion; the client paints a marker at the pointer position and commits.
//! The test measures the FULL input→present cycle and asserts:
//!
//!   * the marker frame's present serial is at most `baseline + 2` — the input-driven render reached the
//!     screen within a BOUNDED number of present cycles (event-driven redraw is prompt, not a tick behind);
//!   * the wall-clock time from inject to the captured marker frame is under a generous ceiling (the
//!     structure is deterministic; the ceiling only guards against a genuine stall);
//!   * at most 2 presents happened after the baseline (no runaway re-render).
//!
//! A companion KEY event is injected and asserted delivered, proving the key path drives the same seam.
//! This is the core "no input-to-present delay" proof.

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
const BASE: [u8; 4] = [0x22, 0x24, 0x28, 0xFF];
const MARKER: [u8; 4] = [0x30, 0xE0, 0x90, 0xFF];
const MK: i32 = 12;
const PX: i32 = 120;
const PY: i32 = 80;
const KEY_A: u32 = 30;

/// Generous wall-clock ceiling for the whole input→present cycle. The structure is deterministic (bounded
/// present cycles); this only fails on a genuine stall, so it is set far above the real (sub-ms) latency.
const LATENCY_CEILING: Duration = Duration::from_millis(1500);

struct App {
    surface: WlSurface,
    base_buffer: WlBuffer,
    marker_buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    marker_drawn: bool,
    keys: u32,
}

#[test]
fn input_to_present_latency() {
    let h = Harness::start("input_to_present_latency");

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
    toplevel.set_title("demo-latency".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(), base_buffer: base_buffer.clone(), marker_buffer: marker_buffer.clone(),
        drawn: false, frame_done: false, marker_drawn: false, keys: 0,
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
    let _ = queue.roundtrip(&mut app);

    // ---- the measured cycle: inject → receive → commit response → present ----
    let t0 = Instant::now();
    h.input_tx.send(InputCommand::FocusTopmostKeyboard).unwrap();
    h.input_tx.send(InputCommand::PointerMotion { x: PX as f64, y: PY as f64 }).unwrap();
    h.input_tx.send(InputCommand::Key { keycode: KEY_A, pressed: true }).unwrap();
    h.input_tx.send(InputCommand::Key { keycode: KEY_A, pressed: false }).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut marker_frame = None;
    let mut t1 = None;
    while marker_frame.is_none() {
        let _ = queue.roundtrip(&mut app);
        if let Some(f) = h.captures.lock().unwrap().iter().rev().find(|f| f.pixel_is(PX, PY, MARKER)).cloned() {
            t1 = Some(Instant::now());
            marker_frame = Some(f);
        }
        assert!(Instant::now() < deadline, "input-driven render never presented (STALL)");
        std::thread::sleep(Duration::from_millis(2));
    }
    let mf = marker_frame.unwrap();
    let elapsed = t1.unwrap().duration_since(t0);
    let cycles = mf.serial - baseline;

    // ---- (a) bounded present cycles ----
    assert!(cycles >= 1 && cycles <= 2, "input→present bounded to <=2 cycles (serial delta {cycles})");
    // ---- (b) generous wall-clock ceiling ----
    assert!(elapsed < LATENCY_CEILING, "input→present latency {elapsed:?} exceeded ceiling {LATENCY_CEILING:?}");
    // ---- (c) no runaway re-render ----
    let after_baseline = h.captures.lock().unwrap().iter().filter(|f| f.serial > baseline).count();
    assert!(after_baseline <= 2, "at most 2 presents after baseline (got {after_baseline})");
    // ---- (d) the key path was delivered ----
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.keys < 2 {
        let _ = queue.roundtrip(&mut app);
        assert!(Instant::now() < deadline, "key events not delivered ({} of 2)", app.keys);
        std::thread::sleep(Duration::from_millis(2));
    }

    eprintln!("input_to_present: cycles={cycles} wall={elapsed:?} (ceiling {LATENCY_CEILING:?})");
    save_frame("input_to_present_latency-marker", &mf);
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
