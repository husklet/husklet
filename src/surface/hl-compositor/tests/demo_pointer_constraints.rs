//! DEMO — `pointer_constraints` (`zwp_locked_pointer_v1`: the pointer is locked in place).
//!
//! A client maps a toplevel, creates a `wl_pointer` (+ a `zwp_relative_pointer_v1` to observe motion while
//! locked), and binds `zwp_pointer_constraints_v1`. The test moves the pointer over the surface (enter),
//! then the client calls `lock_pointer(surface, pointer)`. The adapter — the surface holding pointer focus
//! — activates the constraint, so the client receives `zwp_locked_pointer_v1.locked`. The test then injects
//! further absolute moves and asserts the constraint is HONORED:
//!
//!   * the client received the `locked` event (the compositor engaged the lock);
//!   * after locking, NO further `wl_pointer.motion` arrives — the absolute pointer position is frozen at
//!     the lock point (the defining behavior of a locked pointer);
//!   * relative motion KEEPS flowing (`zwp_relative_pointer_v1.relative_motion` with the exact deltas), so
//!     the client can still drive a virtual cursor / camera while the real pointer is pinned.
//!
//! This is the pointer-lock experience FPS games and pointer-lock web content rely on. Proves the adapter's
//! newly-wired pointer-constraints global both engages the lock and enforces the frozen absolute position.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
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
const COLOR: [u8; 4] = [0x40, 0x60, 0xB0, 0xFF];

const ENTER: (f64, f64) = (100.0, 75.0);
/// Moves injected AFTER the lock engages. Each must yield an exact relative delta but NO absolute motion.
const LOCKED_MOVES: &[(f64, f64)] = &[(130.0, 75.0), (130.0, 110.0), (60.0, 40.0)];

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    /// Absolute `wl_pointer.motion` positions received, in order (surface-local).
    motions: Vec<(f64, f64)>,
    entered: bool,
    locked: bool,
    /// Relative deltas received, in order.
    rel: Vec<(f64, f64)>,
}

#[test]
fn pointer_constraints() {
    let h = Harness::start("pointer_constraints");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    let rel_mgr: ZwpRelativePointerManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("relative_pointer_manager");
    // The newly-wired global under test.
    let constraints: ZwpPointerConstraintsV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_pointer_constraints_v1 advertised");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "pc", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-pointer-constraints".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
        motions: Vec::new(),
        entered: false,
        locked: false,
        rel: Vec::new(),
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

    let pointer: WlPointer = seat.get_pointer(&qh, ());
    let _rel: ZwpRelativePointerV1 = rel_mgr.get_relative_pointer(&pointer, &qh, ());
    let _ = queue.roundtrip(&mut app);

    // Enter the surface so the pointer has focus (required for the compositor to engage the lock).
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: ENTER.0,
            y: ENTER.1,
        })
        .expect("enter motion");
    pump_while(&mut queue, &mut app, 5, |a| a.entered);
    assert!(app.entered, "pointer entered the surface");
    let motions_at_lock = app.motions.len();
    let rel_at_lock = app.rel.len();

    // ---- lock the pointer to the whole surface ----
    let _lock: ZwpLockedPointerV1 =
        constraints.lock_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ());
    pump_while(&mut queue, &mut app, 5, |a| a.locked);
    assert!(
        app.locked,
        "the compositor engaged the lock (zwp_locked_pointer_v1.locked received)"
    );

    // ---- inject moves while locked ----
    for &(x, y) in LOCKED_MOVES {
        h.input_tx
            .send(InputCommand::PointerMotion { x, y })
            .expect("locked motion");
    }
    pump_while(&mut queue, &mut app, 5, |a| {
        a.rel.len() >= rel_at_lock + LOCKED_MOVES.len()
    });

    // The absolute position is FROZEN: no wl_pointer.motion arrived after the lock engaged.
    assert_eq!(
        app.motions.len(),
        motions_at_lock,
        "pointer position locked: no absolute motion after lock, got {:?}",
        &app.motions[motions_at_lock..]
    );

    // Relative motion kept flowing with EXACT deltas (consecutive differences of [ENTER, ...LOCKED_MOVES]).
    let mut positions = vec![ENTER];
    positions.extend_from_slice(LOCKED_MOVES);
    let expected: Vec<(f64, f64)> = positions
        .windows(2)
        .map(|w| (w[1].0 - w[0].0, w[1].1 - w[0].1))
        .collect();
    let got_locked = &app.rel[rel_at_lock..];
    assert_eq!(
        got_locked.len(),
        expected.len(),
        "one relative delta per locked move, got {got_locked:?}"
    );
    for (i, (g, e)) in got_locked.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            g, e,
            "relative delta #{i} while locked exact: got {g:?} want {e:?}"
        );
    }

    save_frame("pointer_constraints-window", &mapped);
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
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => app.motions.push((surface_x, surface_y)),
            _ => {}
        }
    }
}
impl Dispatch<ZwpLockedPointerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpLockedPointerV1,
        e: <ZwpLockedPointerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_locked_pointer_v1::Event::Locked = e {
            app.locked = true;
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
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = e {
            app.rel.push((dx, dy));
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
    WlRegion,
    XdgToplevel,
    ZwpRelativePointerManagerV1,
    ZwpPointerConstraintsV1
);
