//! DEMO — `pointer_enter_leave_rerender` (the enter/leave interaction loop, closed on pixels).
//!
//! The pointer-focus handoff, proven end to end: inject a motion INSIDE the toplevel → the client
//! receives `wl_pointer.enter` (naming ITS surface, at the exact surface-local coord) + `motion`, draws a
//! marker, and the compositor composites the marker. Then inject a motion OUTSIDE the toplevel → the
//! client receives `wl_pointer.leave` (naming its surface), redraws a clean frame, and the compositor
//! composites the marker GONE. Moving back in re-enters and re-draws. This closes the loop the raw
//! `demo_input_rerender` (motion only) leaves open: it asserts `wl_pointer.leave` is delivered with the
//! right surface, and that the client's REACTION to the leave (clearing the cursor marker) actually
//! re-composites.

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
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const BASE: [u8; 4] = [0x28, 0x28, 0x28, 0xFF];
const MARKER: [u8; 4] = [0xF0, 0x30, 0xC0, 0xFF];
const M: i32 = 10;
const IN: (i32, i32) = (70, 55); // a point inside the toplevel
const OUT: (i32, i32) = (W + 40, H + 40); // a point off the toplevel entirely

/// One `wl_pointer` focus event, recorded in arrival order (surface-local coord for enter).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PtrEv {
    Enter(i32, i32),
    Leave,
    Motion(i32, i32),
}

struct App {
    surface: WlSurface,
    base: WlBuffer,
    marker: WlBuffer,
    drawn: bool,
    frame_done: bool,
    ev: Vec<PtrEv>,
    /// Draw generation, bumped each redraw so a captured frame can be correlated to a reaction.
    redraws: u32,
}

impl App {
    fn draw(&mut self, marker: bool, qh: &QueueHandle<App>) {
        self.surface
            .attach(Some(if marker { &self.marker } else { &self.base }), 0, 0);
        self.surface.damage(0, 0, W, H);
        let _cb: WlCallback = self.surface.frame(qh, ());
        self.surface.commit();
        self.redraws += 1;
    }
    fn enters(&self) -> usize {
        self.ev
            .iter()
            .filter(|e| matches!(e, PtrEv::Enter(..)))
            .count()
    }
    fn leaves(&self) -> usize {
        self.ev.iter().filter(|e| matches!(e, PtrEv::Leave)).count()
    }
}

#[test]
fn pointer_enter_leave_rerender() {
    let h = Harness::start("pointer_enter_leave");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let mut mk = solid(W, H, BASE);
    fill_rect(&mut mk, W, H, IN.0 - M / 2, IN.1 - M / 2, M, M, MARKER);
    let marker = make_buffer(&shm, &qh, &h.runtime_dir, "mk", W, H, &mk);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-enter-leave".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        base: base.clone(),
        marker,
        drawn: false,
        frame_done: false,
        ev: Vec::new(),
        redraws: 0,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W
            && f.pixel_is(1, 1, BASE))
        .is_some(),
        "base frame never composited",
    );

    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _ = queue.roundtrip(&mut app);
    h.input_tx.send(InputCommand::FocusTopmostKeyboard).ok();

    // ---- ENTER: motion inside → enter (exact surface-local coord) + marker composited ----
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: IN.0 as f64,
            y: IN.1 as f64,
        })
        .expect("motion in");
    let entered = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(IN.0, IN.1, MARKER)
    })
    .expect("marker never composited after enter");
    assert_eq!(
        entered.pixel(IN.0, IN.1).unwrap(),
        MARKER,
        "marker present where the pointer entered"
    );
    assert_eq!(app.enters(), 1, "exactly one wl_pointer.enter delivered");
    assert!(
        app.ev.contains(&PtrEv::Enter(IN.0, IN.1)),
        "enter carried the exact surface-local coord, got {:?}",
        app.ev
    );
    save_frame("pointer_enter_leave-entered", &entered);

    // ---- LEAVE: motion off the surface → leave + marker cleared from the composited frame ----
    let before_leave_serial = entered.serial;
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: OUT.0 as f64,
            y: OUT.1 as f64,
        })
        .expect("motion out");
    // The client redraws base on leave; wait for a fresh present with the marker gone.
    let left = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > before_leave_serial && f.width == W && f.pixel_is(IN.0, IN.1, BASE)
    })
    .expect("compositor never re-presented with the marker cleared after leave");
    assert_eq!(
        app.leaves(),
        1,
        "exactly one wl_pointer.leave delivered on exiting the surface"
    );
    assert_eq!(
        left.pixel(IN.0, IN.1).unwrap(),
        BASE,
        "marker cleared where the pointer left"
    );
    save_frame("pointer_enter_leave-left", &left);

    // ---- RE-ENTER: motion back in → a SECOND enter + the marker returns ----
    let before_reenter = left.serial;
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: IN.0 as f64,
            y: IN.1 as f64,
        })
        .expect("motion back in");
    let reentered = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > before_reenter && f.width == W && f.pixel_is(IN.0, IN.1, MARKER)
    })
    .expect("marker never re-composited after re-enter");
    assert_eq!(
        app.enters(),
        2,
        "a second wl_pointer.enter delivered on re-entry"
    );
    assert_eq!(
        reentered.pixel(IN.0, IN.1).unwrap(),
        MARKER,
        "marker returned on re-entry"
    );

    // Exact focus-event sequence: enter, leave, enter (the pointer crossed the boundary out then back).
    let focus_seq: Vec<PtrEv> = app
        .ev
        .iter()
        .copied()
        .filter(|e| !matches!(e, PtrEv::Motion(..)))
        .collect();
    assert_eq!(
        focus_seq,
        vec![
            PtrEv::Enter(IN.0, IN.1),
            PtrEv::Leave,
            PtrEv::Enter(IN.0, IN.1)
        ],
        "focus events are exactly enter,leave,enter",
    );

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
                app.draw(false, qh);
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
        qh: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } if surface.id() == app.surface.id() => {
                app.ev.push(PtrEv::Enter(
                    surface_x.round() as i32,
                    surface_y.round() as i32,
                ));
                app.draw(true, qh); // react: show the cursor marker
            }
            wl_pointer::Event::Leave { surface, .. } if surface.id() == app.surface.id() => {
                app.ev.push(PtrEv::Leave);
                app.draw(false, qh); // react: clear the cursor marker
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                app.ev.push(PtrEv::Motion(
                    surface_x.round() as i32,
                    surface_y.round() as i32,
                ));
            }
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
