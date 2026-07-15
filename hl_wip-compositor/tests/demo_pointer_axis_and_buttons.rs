//! DEMO 9 — `pointer_axis_and_buttons` (scroll + a button chord arrive with exact values + frame grouping).
//!
//! A client maps a toplevel + creates a pointer. The test moves the pointer over the surface (enter),
//! then injects a two-axis scroll (a single logical scroll event carrying BOTH a horizontal and a
//! vertical component) followed by a two-button chord (BTN_LEFT then BTN_RIGHT, both pressed). It asserts
//! on the WIRE:
//!
//!   * `wl_pointer.axis` for VerticalScroll and HorizontalScroll with the EXACT injected values
//!     (including sign — a leftward/negative horizontal is not mistaken for rightward);
//!   * both axis events fall in ONE `wl_pointer.frame` group (the scroll is one logical event), while
//!     each button lands in its own frame;
//!   * both `wl_pointer.button` events (BTN_LEFT + BTN_RIGHT, pressed) are delivered.
//!
//! Proves the compositor's scroll value/sign and event framing are faithful — a toolkit's smooth-scroll
//! and chord handling see coherent, correctly-grouped events.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_pointer::{self, Axis, ButtonState, WlPointer}, wl_registry::WlRegistry, wl_seat::WlSeat,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 180;
const H: i32 = 140;
const COLOR: [u8; 4] = [0x80, 0x30, 0x90, 0xFF];
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
// Distinct magnitude + sign so an axis swap or sign flip is caught.
const VSCROLL: f64 = 12.0; // downward
const HSCROLL: f64 = -8.0; // leftward (negative)

#[derive(Debug, Clone, Copy, PartialEq)]
enum PtrEv {
    Enter,
    AxisV(f64),
    AxisH(f64),
    Button(u32, bool),
    Frame,
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    ev: Vec<PtrEv>,
}

impl App {
    fn has(&self, e: PtrEv) -> bool {
        self.ev.contains(&e)
    }
    /// The events of each `wl_pointer.frame` group (split on `Frame`, dropping the trailing partial).
    fn frame_groups(&self) -> Vec<Vec<PtrEv>> {
        let mut groups = Vec::new();
        let mut cur = Vec::new();
        for e in &self.ev {
            if *e == PtrEv::Frame {
                groups.push(std::mem::take(&mut cur));
            } else {
                cur.push(*e);
            }
        }
        groups
    }
}

#[test]
fn pointer_axis_and_buttons() {
    let h = Harness::start("pointer_axis_buttons");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "ptr", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-axis".into());
    surface.commit();

    let mut app = App { surface: surface.clone(), buffer: buffer.clone(), drawn: false, frame_done: false, ev: Vec::new() };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let mapped = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, COLOR))
        .expect("mapped frame never composited");

    // Create the pointer, then move it over the surface so scroll + buttons route to it (enter).
    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _ = queue.roundtrip(&mut app);
    h.input_tx.send(InputCommand::PointerMotion { x: 80.0, y: 60.0 }).expect("enter motion");
    pump_while(&mut queue, &mut app, 5, |a| a.has(PtrEv::Enter));
    assert!(app.has(PtrEv::Enter), "pointer entered the surface");
    let events_before = app.ev.len();

    // ---- one two-axis scroll ----
    h.input_tx.send(InputCommand::PointerAxis { horizontal: HSCROLL, vertical: VSCROLL }).expect("axis");
    pump_while(&mut queue, &mut app, 5, |a| a.has(PtrEv::AxisV(VSCROLL)) && a.has(PtrEv::AxisH(HSCROLL)));
    assert!(app.has(PtrEv::AxisV(VSCROLL)), "vertical scroll value delivered exactly (with sign)");
    assert!(app.has(PtrEv::AxisH(HSCROLL)), "horizontal scroll value delivered exactly (with sign)");

    // ---- two-button chord ----
    h.input_tx.send(InputCommand::PointerButton { button: BTN_LEFT, pressed: true }).expect("left down");
    h.input_tx.send(InputCommand::PointerButton { button: BTN_RIGHT, pressed: true }).expect("right down");
    pump_while(&mut queue, &mut app, 5, |a| a.has(PtrEv::Button(BTN_LEFT, true)) && a.has(PtrEv::Button(BTN_RIGHT, true)));
    assert!(app.has(PtrEv::Button(BTN_LEFT, true)), "BTN_LEFT pressed delivered");
    assert!(app.has(PtrEv::Button(BTN_RIGHT, true)), "BTN_RIGHT pressed delivered");

    // ---- frame grouping ----
    let groups = app.frame_groups();
    // The scroll's two axes share ONE frame group.
    let scroll_group = groups.iter().find(|g| g.contains(&PtrEv::AxisV(VSCROLL)))
        .expect("a frame group contains the vertical axis");
    assert!(scroll_group.contains(&PtrEv::AxisH(HSCROLL)),
        "both axes are grouped in ONE wl_pointer.frame, got group {scroll_group:?}");
    assert!(!scroll_group.iter().any(|e| matches!(e, PtrEv::Button(..))),
        "the scroll frame carries no button events, got {scroll_group:?}");
    // Each button lands in its own frame group (no two Button events share a group).
    for g in &groups {
        let btns = g.iter().filter(|e| matches!(e, PtrEv::Button(..))).count();
        assert!(btns <= 1, "each wl_pointer.frame carries at most one button, got {g:?}");
    }
    // Frames actually happened (v5+ grouping is live, not a degenerate single stream).
    assert!(app.ev.iter().filter(|e| **e == PtrEv::Frame).count() >= 3,
        "at least three frames (scroll + two buttons), got {:?}", app.ev);
    assert!(app.ev.len() > events_before, "events accumulated after enter");

    save_frame("pointer_axis_buttons-window", &mapped);

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
                app.surface.attach(Some(&app.buffer), 0, 0);
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
impl Dispatch<WlPointer, ()> for App {
    fn event(app: &mut Self, _: &WlPointer, e: <WlPointer as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_pointer::Event::Enter { .. } => app.ev.push(PtrEv::Enter),
            wl_pointer::Event::Axis { axis, value, .. } => match axis {
                WEnum::Value(Axis::VerticalScroll) => app.ev.push(PtrEv::AxisV(value)),
                WEnum::Value(Axis::HorizontalScroll) => app.ev.push(PtrEv::AxisH(value)),
                _ => {}
            },
            wl_pointer::Event::Button { button, state, .. } => {
                app.ev.push(PtrEv::Button(button, matches!(state, WEnum::Value(ButtonState::Pressed))));
            }
            wl_pointer::Event::Frame => app.ev.push(PtrEv::Frame),
            _ => {}
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, XdgToplevel);
