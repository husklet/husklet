//! DEMO — `relative_pointer` (`zwp_relative_pointer_v1`: unaccelerated relative motion deltas).
//!
//! A client maps a toplevel, creates a `wl_pointer`, binds `zwp_relative_pointer_manager_v1`, and calls
//! `get_relative_pointer(pointer)`. The test moves the pointer over the surface (enter) and then injects a
//! SEQUENCE of absolute moves; it asserts the `zwp_relative_pointer_v1` receives one
//! `relative_motion(dx, dy)` per move carrying the EXACT delta between consecutive positions — including
//! sign (a leftward/upward move is a negative delta, not mistaken for its mirror) — and that the
//! unaccelerated delta equals the accelerated one (the headless adapter applies no pointer acceleration).
//!
//! This is what FPS games / 3D viewports / pointer-lock web content consume: raw motion deltas independent
//! of the absolute cursor position. Proves the adapter's newly-wired relative-pointer global delivers the
//! delta stream faithfully and in order.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_pointer::{self, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 200;
const H: i32 = 150;
const COLOR: [u8; 4] = [0x20, 0x90, 0x50, 0xFF];

/// The enter point + the sequence of absolute moves that follow it. Relative motion is delivered only to a
/// surface that ALREADY holds pointer focus, and focus is established by the enter move itself — so the
/// enter move yields no relative delta, and each subsequent move yields the delta from the previous
/// position (starting from `ENTER`).
const ENTER: (f64, f64) = (100.0, 75.0);
const MOVES: &[(f64, f64)] = &[(115.0, 75.0), (115.0, 95.0), (90.0, 60.0), (91.0, 61.0)];

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    entered: bool,
    /// Every `relative_motion` delta received, in order: `(dx, dy, dx_unaccel, dy_unaccel)`.
    deltas: Vec<(f64, f64, f64, f64)>,
}

#[test]
fn relative_pointer() {
    let h = Harness::start("relative_pointer");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    // The newly-wired global: without this bind() would fail (proof it is advertised).
    let rel_mgr: ZwpRelativePointerManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_relative_pointer_manager_v1 advertised");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "rp", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-relative-pointer".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
        entered: false,
        deltas: Vec::new(),
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

    // Create the pointer + the relative pointer atop it.
    let pointer: WlPointer = seat.get_pointer(&qh, ());
    let _rel: ZwpRelativePointerV1 = rel_mgr.get_relative_pointer(&pointer, &qh, ());
    let _ = queue.roundtrip(&mut app);

    // Enter the surface: the first move produces a relative delta equal to ENTER (from initial (0,0)).
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: ENTER.0,
            y: ENTER.1,
        })
        .expect("enter motion");
    pump_while(&mut queue, &mut app, 5, |a| {
        a.entered && !a.deltas.is_empty()
    });
    assert!(app.entered, "pointer entered the surface");

    // Inject the move sequence.
    for &(x, y) in MOVES {
        h.input_tx
            .send(InputCommand::PointerMotion { x, y })
            .expect("motion");
    }
    // Wait until every injected MOVE's delta has arrived (the enter move yields no relative delta).
    let want = MOVES.len();
    pump_while(&mut queue, &mut app, 5, |a| a.deltas.len() >= want);

    // ---- EXACT deltas: consecutive differences of [ENTER, ...MOVES] (focus established at ENTER) ----
    let mut positions = vec![ENTER];
    positions.extend_from_slice(MOVES);
    let expected: Vec<(f64, f64)> = positions
        .windows(2)
        .map(|w| (w[1].0 - w[0].0, w[1].1 - w[0].1))
        .collect();

    assert_eq!(
        app.deltas.len(),
        expected.len(),
        "one relative_motion per move, got {:?}",
        app.deltas
    );
    for (i, ((dx, dy, dxu, dyu), (ex, ey))) in app.deltas.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            (*dx, *dy),
            (*ex, *ey),
            "relative delta #{i} exact (with sign): got ({dx},{dy}) want ({ex},{ey})"
        );
        // The headless adapter applies no acceleration: unaccelerated == accelerated.
        assert_eq!(
            (*dxu, *dyu),
            (*ex, *ey),
            "unaccelerated delta #{i} equals accelerated"
        );
    }

    save_frame("relative_pointer-window", &mapped);
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
        if let wl_pointer::Event::Enter { .. } = e {
            app.entered = true;
        }
    }
}
impl Dispatch<ZwpRelativePointerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpRelativePointerV1,
        e: <ZwpRelativePointerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
            ..
        } = e
        {
            app.deltas.push((dx, dy, dx_unaccel, dy_unaccel));
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
    ZwpRelativePointerManagerV1
);
