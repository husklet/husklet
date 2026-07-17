//! DEMO — `pointer_axis_discrete` (a mouse WHEEL delivers smooth value + discrete notches + source).
//!
//! A real mouse wheel scroll is not just a smooth delta — it carries a DISCRETE notch count and an
//! `axis_source(wheel)` tag. The test injects a two-axis discrete scroll (vertical: value +15, one notch
//! down; horizontal: value -10, one notch left) via the host seam's discrete-axis path and asserts, on
//! the WIRE, that in ONE `wl_pointer.frame` the client receives:
//!
//!   * `wl_pointer.axis` with the EXACT smooth values (+15 vertical, -10 horizontal — sign preserved);
//!   * `wl_pointer.axis_source(wheel)`;
//!   * the discrete notch on each axis — `wl_pointer.axis_value120` (client v8+, 120 units = one notch)
//!     or the legacy `wl_pointer.axis_discrete` (v5-7), with the exact signed step count.
//!
//! Proves the compositor's wheel framing is complete: a toolkit that page-scrolls on discrete notches
//! (not accumulated smooth deltas) sees a coherent, correctly-signed, single-frame wheel event.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_pointer::{self, Axis, AxisSource, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 180;
const H: i32 = 140;
const COLOR: [u8; 4] = [0x30, 0x50, 0x80, 0xFF];
const VVAL: f64 = 15.0; // downward smooth
const HVAL: f64 = -10.0; // leftward smooth (negative)
const V120: i32 = 120; // one notch down
const H120: i32 = -120; // one notch left

#[derive(Debug, Clone, Copy, PartialEq)]
enum PtrEv {
    AxisV(u64), // vertical smooth value (bit-encoded so PartialEq works)
    AxisH(u64),
    Source(u32), // axis_source as its wire discriminant
    V120(i32),   // axis_value120 vertical
    H120(i32),
    Discrete(u32, i32), // axis_discrete (axis wire, steps) — legacy v5-7 fallback
    Frame,
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    ev: Vec<PtrEv>,
    entered: bool,
}

impl App {
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
fn pointer_axis_discrete() {
    let h = Harness::start("pointer_axis_discrete");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "px", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-axis-discrete".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
        ev: Vec::new(),
        entered: false,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let mapped = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, COLOR)
    })
    .expect("mapped frame never composited");

    // Move over the surface so the wheel routes to it.
    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _ = queue.roundtrip(&mut app);
    h.input_tx
        .send(InputCommand::PointerMotion { x: 90.0, y: 70.0 })
        .expect("enter");
    pump_while(&mut queue, &mut app, 5, |a| a.entered);
    assert!(app.entered, "pointer entered the surface");
    let before = app.ev.len();

    // ---- one two-axis DISCRETE wheel scroll ----
    h.input_tx
        .send(InputCommand::PointerAxisDiscrete {
            horizontal: HVAL,
            vertical: VVAL,
            h120: H120,
            v120: V120,
        })
        .expect("discrete axis");
    pump_while(&mut queue, &mut app, 5, |a| {
        a.ev[before..].iter().any(|e| *e == PtrEv::Frame)
    });

    let groups = app.frame_groups();
    let wheel = groups
        .iter()
        .find(|g| g.iter().any(|e| matches!(e, PtrEv::AxisV(_))))
        .expect("a frame group carried the vertical wheel axis");

    // Smooth values with exact sign.
    assert!(
        wheel.contains(&PtrEv::AxisV(VVAL.to_bits())),
        "vertical smooth value +15 delivered, got {wheel:?}"
    );
    assert!(
        wheel.contains(&PtrEv::AxisH(HVAL.to_bits())),
        "horizontal smooth value -10 delivered, got {wheel:?}"
    );
    // Wheel source tag.
    assert!(
        wheel.contains(&PtrEv::Source(u32::from(AxisSource::Wheel))),
        "axis_source(wheel) delivered, got {wheel:?}"
    );
    // Discrete notch: value120 (v8+) OR the legacy axis_discrete (v5-7). Accept either, exact + signed.
    let v_notch = wheel.contains(&PtrEv::V120(V120))
        || wheel.contains(&PtrEv::Discrete(u32::from(Axis::VerticalScroll), 1));
    let h_notch = wheel.contains(&PtrEv::H120(H120))
        || wheel.contains(&PtrEv::Discrete(u32::from(Axis::HorizontalScroll), -1));
    assert!(
        v_notch,
        "vertical discrete notch (+1 / value120 +120) delivered, got {wheel:?}"
    );
    assert!(
        h_notch,
        "horizontal discrete notch (-1 / value120 -120) delivered, got {wheel:?}"
    );
    // The whole wheel event is ONE frame (source + both axes + both notches grouped together).
    assert!(
        wheel.iter().any(|e| matches!(e, PtrEv::AxisV(_)))
            && wheel.iter().any(|e| matches!(e, PtrEv::AxisH(_))),
        "both axes share one wl_pointer.frame, got {wheel:?}"
    );

    save_frame("pointer_axis_discrete-window", &mapped);

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
impl Dispatch<WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        e: <WlPointer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter { .. } => app.entered = true,
            wl_pointer::Event::Axis { axis, value, .. } => match axis {
                WEnum::Value(Axis::VerticalScroll) => app.ev.push(PtrEv::AxisV(value.to_bits())),
                WEnum::Value(Axis::HorizontalScroll) => app.ev.push(PtrEv::AxisH(value.to_bits())),
                _ => {}
            },
            wl_pointer::Event::AxisSource { axis_source } => {
                if let WEnum::Value(s) = axis_source {
                    app.ev.push(PtrEv::Source(u32::from(s)));
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => match axis {
                WEnum::Value(Axis::VerticalScroll) => app.ev.push(PtrEv::V120(value120)),
                WEnum::Value(Axis::HorizontalScroll) => app.ev.push(PtrEv::H120(value120)),
                _ => {}
            },
            wl_pointer::Event::AxisDiscrete { axis, discrete } => {
                if let WEnum::Value(a) = axis {
                    app.ev.push(PtrEv::Discrete(u32::from(a), discrete));
                }
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
ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSeat,
    XdgToplevel
);
