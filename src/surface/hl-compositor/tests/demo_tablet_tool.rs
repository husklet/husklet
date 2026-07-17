//! DEMO (batch-input) — `tablet_tool` (real `zwp_tablet_manager_v2` / `zwp_tablet_tool_v2` stylus:
//! proximity, tip contact, motion, pressure — exact values).
//!
//! A mapped toplevel binds `zwp_tablet_manager_v2`, requests a tablet seat, and receives the advertised
//! tablet + pen tool. The host stylus seam then drives a full pen interaction; the test asserts the client
//! receives, in exact order and with exact values:
//!
//!   * `proximity_in` naming OUR surface (the pen is hovering the toplevel), followed by the mandatory
//!     first `motion` at the hover coordinate;
//!   * a hover `motion` at exact `(x, y)` carrying an exact `pressure` (0.0–1.0 → 0–65535 wire units);
//!   * `down` (tip contact), a drawing `motion` + `pressure`, `up` (tip lift);
//!   * `proximity_out` (the pen left).
//!
//! This proves the tablet adapter delivers genuine stylus input with per-axis fidelity, not just a bound
//! global.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::ZwpTabletV2,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x18, 0x20, 0x28, 0xFF];

/// Recorded `zwp_tablet_tool_v2` interaction event, exact values.
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    ProximityIn { on_surface: bool },
    Motion { x: i32, y: i32 },
    Pressure { v: u32 },
    Down,
    Up,
    ProximityOut,
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    tool_added: bool,
    events: Vec<Ev>,
}

#[test]
fn tablet_tool() {
    let h = Harness::start("tablet_tool");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    let tablet_mgr: ZwpTabletManagerV2 =
        globals.bind(&qh, 1..=1, ()).expect("zwp_tablet_manager_v2");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-tablet".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
        tool_added: false,
        events: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, BASE)
    })
    .expect("base frame never composited");

    // Request the tablet seat; the compositor advertises the tablet + pen tool on it.
    let _tablet_seat: ZwpTabletSeatV2 = tablet_mgr.get_tablet_seat(&seat, &qh, ());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.tool_added {
        assert!(Instant::now() < deadline, "tablet tool never advertised");
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch tool_added");
    }

    // ---- drive a full stylus interaction ----
    h.input_tx
        .send(InputCommand::TabletToolProximityIn { x: 60.0, y: 40.0 })
        .unwrap();
    h.input_tx
        .send(InputCommand::TabletToolMotion {
            x: 70.0,
            y: 55.0,
            pressure: 0.6,
        })
        .unwrap();
    h.input_tx.send(InputCommand::TabletToolTipDown).unwrap();
    h.input_tx
        .send(InputCommand::TabletToolMotion {
            x: 80.0,
            y: 65.0,
            pressure: 0.8,
        })
        .unwrap();
    h.input_tx.send(InputCommand::TabletToolTipUp).unwrap();
    h.input_tx
        .send(InputCommand::TabletToolProximityOut)
        .unwrap();

    // Expected exact interaction events (frames elided; pressure = round(p*65535)).
    let expected = vec![
        Ev::ProximityIn { on_surface: true },
        Ev::Motion { x: 60, y: 40 }, // proximity_in's mandatory first motion at the hover point
        Ev::Motion { x: 70, y: 55 },
        Ev::Pressure { v: 39321 }, // round(0.6 * 65535)
        Ev::Down,
        Ev::Motion { x: 80, y: 65 },
        Ev::Pressure { v: 52428 }, // round(0.8 * 65535)
        Ev::Up,
        Ev::ProximityOut,
    ];
    let want = expected.len();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.events.len() < want {
        let _ = queue.roundtrip(&mut app);
        assert!(
            Instant::now() < deadline,
            "tablet events incomplete: {:?}",
            app.events
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.events, expected,
        "exact stylus interaction: proximity, motion, pressure, tip, out"
    );

    h.shutdown();
}

// ---------- dispatch plumbing ----------
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
impl Dispatch<ZwpTabletSeatV2, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpTabletSeatV2,
        _: <ZwpTabletSeatV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
    wayland_client::event_created_child!(App, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
    ]);
}
impl Dispatch<ZwpTabletV2, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpTabletV2,
        _: <ZwpTabletV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<ZwpTabletToolV2, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpTabletToolV2,
        e: <ZwpTabletToolV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_tablet_tool_v2::Event;
        match e {
            Event::Done => app.tool_added = true,
            Event::ProximityIn { surface, .. } => {
                app.events.push(Ev::ProximityIn {
                    on_surface: surface == app.surface,
                });
            }
            Event::ProximityOut => app.events.push(Ev::ProximityOut),
            Event::Down { .. } => app.events.push(Ev::Down),
            Event::Up => app.events.push(Ev::Up),
            Event::Motion { x, y } => app.events.push(Ev::Motion {
                x: x.floor() as i32,
                y: y.floor() as i32,
            }),
            Event::Pressure { pressure } => app.events.push(Ev::Pressure { v: pressure }),
            _ => {}
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
    WlSeat,
    XdgToplevel,
    ZwpTabletManagerV2
);
