//! DEMO (batch-input) — `touch_multitouch` (real `wl_touch` multi-touch: distinct points, exact coords,
//! frame grouping, and cancel).
//!
//! A mapped toplevel binds `wl_touch`. The host touch seam injects TWO independent touch points (distinct
//! ids at distinct coordinates) inside one atomic frame, then moves one, then lifts both, then drives a
//! fresh point that is CANCELLED. The test asserts the client receives, in exact order and with exact
//! id/x/y:
//!
//!   * two `wl_touch.down` (ids 0 and 1 at their injected surface-local coordinates) grouped by a single
//!     `wl_touch.frame` — the multi-touch batch;
//!   * a `wl_touch.motion` for id 0 at its new coordinate, framed;
//!   * two `wl_touch.up` (ids 0 and 1), framed;
//!   * a `wl_touch.cancel` ending a later sequence.
//!
//! This proves the adapter's `InputCommand::Touch*` seam delivers genuine multi-touch over the wire with
//! per-point ids, exact placement, and correct frame grouping.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
    wl_touch::{self, WlTouch},
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x20, 0x28, 0x30, 0xFF];

/// One recorded `wl_touch` event, exact values (fixed24 coords floored to whole pixels — the demo injects
/// integer coordinates so the round-trip is exact).
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Down { id: i32, x: i32, y: i32 },
    Motion { id: i32, x: i32, y: i32 },
    Up { id: i32 },
    Frame,
    Cancel,
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    events: Vec<Ev>,
}

#[test]
fn touch_multitouch() {
    let h = Harness::start("touch_multitouch");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-touch".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
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

    // The touch object — injected touch routes to this live client object.
    let _touch: WlTouch = seat.get_touch(&qh, ());
    let _ = queue.roundtrip(&mut app);

    // ---- multi-touch batch: two distinct points, one frame ----
    h.input_tx
        .send(InputCommand::TouchDown {
            id: 0,
            x: 30.0,
            y: 40.0,
        })
        .unwrap();
    h.input_tx
        .send(InputCommand::TouchDown {
            id: 1,
            x: 90.0,
            y: 20.0,
        })
        .unwrap();
    h.input_tx.send(InputCommand::TouchFrame).unwrap();
    // move point 0, framed
    h.input_tx
        .send(InputCommand::TouchMotion {
            id: 0,
            x: 35.0,
            y: 45.0,
        })
        .unwrap();
    h.input_tx.send(InputCommand::TouchFrame).unwrap();
    // lift both, framed
    h.input_tx.send(InputCommand::TouchUp { id: 0 }).unwrap();
    h.input_tx.send(InputCommand::TouchUp { id: 1 }).unwrap();
    h.input_tx.send(InputCommand::TouchFrame).unwrap();
    // a fresh point, then a compositor-driven cancel (cancel is itself the frame boundary, so no
    // intervening TouchFrame — smithay coalesces a framed slot out of the cancel set).
    h.input_tx
        .send(InputCommand::TouchDown {
            id: 2,
            x: 50.0,
            y: 50.0,
        })
        .unwrap();
    h.input_tx.send(InputCommand::TouchCancel).unwrap();

    let want = 10; // 2 down + frame + motion + frame + 2 up + frame + down + cancel
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.events.len() < want {
        let _ = queue.roundtrip(&mut app);
        assert!(
            Instant::now() < deadline,
            "touch events incomplete: {:?}",
            app.events
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let expected = vec![
        Ev::Down {
            id: 0,
            x: 30,
            y: 40,
        },
        Ev::Down {
            id: 1,
            x: 90,
            y: 20,
        },
        Ev::Frame,
        Ev::Motion {
            id: 0,
            x: 35,
            y: 45,
        },
        Ev::Frame,
        Ev::Up { id: 0 },
        Ev::Up { id: 1 },
        Ev::Frame,
        Ev::Down {
            id: 2,
            x: 50,
            y: 50,
        },
        Ev::Cancel,
    ];
    assert_eq!(
        app.events, expected,
        "exact multi-touch stream with per-point ids, coords, and frame grouping"
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
impl Dispatch<WlTouch, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlTouch,
        e: <WlTouch as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_touch::Event::Down { id, x, y, .. } => {
                app.events.push(Ev::Down {
                    id,
                    x: x.floor() as i32,
                    y: y.floor() as i32,
                });
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                app.events.push(Ev::Motion {
                    id,
                    x: x.floor() as i32,
                    y: y.floor() as i32,
                });
            }
            wl_touch::Event::Up { id, .. } => app.events.push(Ev::Up { id }),
            wl_touch::Event::Frame => app.events.push(Ev::Frame),
            wl_touch::Event::Cancel => app.events.push(Ev::Cancel),
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
    XdgToplevel
);
