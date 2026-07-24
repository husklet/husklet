//! DEMO — `pointer_drag_stream` (a full press → motions → release drag, every event in order).
//!
//! The canonical drag gesture, closed on the wire AND on pixels. The test injects: a motion to enter, a
//! `BTN_LEFT` press, a STREAM of N intermediate motions, then a `BTN_LEFT` release. It asserts:
//!
//!   * `wl_pointer.enter` then `wl_pointer.button(BTN_LEFT, pressed)` then every one of the N motions IN
//!     ORDER at the exact surface-local coordinates, then `wl_pointer.button(BTN_LEFT, released)`;
//!   * the events that carry a serial (enter, both buttons) have STRICTLY MONOTONIC serials, and the
//!     motion timestamps are non-decreasing;
//!   * the client redraws a marker tracking the cursor on every motion, and successive composited frames
//!     show the marker at each drag position in order (monotonic present serials) — the drag is delivered
//!     completely and in sequence, none dropped or reordered.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_pointer::{self, ButtonState, WlPointer},
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

const W: i32 = 220;
const H: i32 = 160;
const BASE: [u8; 4] = [0x22, 0x22, 0x2A, 0xFF];
const MARKER: [u8; 4] = [0x30, 0xF0, 0x60, 0xFF];
const M: i32 = 10;
const BTN_LEFT: u32 = 0x110;
// The drag path: press point, then four intermediate motions (all inside the surface, distinct cells).
const START: (i32, i32) = (30, 30);
const TRACK: [(i32, i32); 4] = [(90, 40), (150, 80), (110, 120), (50, 100)];

#[derive(Debug, Clone, Copy, PartialEq)]
enum PtrEv {
    Enter(u32),             // serial
    Motion(i32, i32, u32),  // x, y, time
    Button(u32, bool, u32), // button, pressed, serial
}

struct App {
    surface: WlSurface,
    base: WlBuffer,
    markers: Vec<((i32, i32), WlBuffer)>,
    drawn: bool,
    frame_done: bool,
    ev: Vec<PtrEv>,
    redraws: u32,
}

impl App {
    fn draw_marker_at(&mut self, sx: i32, sy: i32, qh: &QueueHandle<App>) {
        if let Some((_, buf)) = self.markers.iter().find(|(p, _)| *p == (sx, sy)) {
            self.surface.attach(Some(buf), 0, 0);
            self.surface.damage(0, 0, W, H);
            let _cb: WlCallback = self.surface.frame(qh, ());
            self.surface.commit();
            self.redraws += 1;
        }
    }
    fn motions(&self) -> Vec<(i32, i32)> {
        self.ev
            .iter()
            .filter_map(|e| match e {
                PtrEv::Motion(x, y, _) => Some((*x, *y)),
                _ => None,
            })
            .collect()
    }
    fn buttons(&self) -> Vec<(u32, bool)> {
        self.ev
            .iter()
            .filter_map(|e| match e {
                PtrEv::Button(b, p, _) => Some((*b, *p)),
                _ => None,
            })
            .collect()
    }
    /// The serials of every event that carries one, in arrival order (enter + both buttons).
    fn serials(&self) -> Vec<u32> {
        self.ev
            .iter()
            .filter_map(|e| match e {
                PtrEv::Enter(s) | PtrEv::Button(_, _, s) => Some(*s),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn pointer_drag_stream() {
    let h = Harness::start("pointer_drag_stream");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    // One pre-drawn buffer per drag cell (START + the four TRACK points), marker centered there.
    let mut markers = Vec::new();
    for (i, &(px, py)) in std::iter::once(&START).chain(TRACK.iter()).enumerate() {
        let mut buf = solid(W, H, BASE);
        fill_rect(&mut buf, W, H, px - M / 2, py - M / 2, M, M, MARKER);
        markers.push((
            (px, py),
            make_buffer(&shm, &qh, &h.runtime_dir, &format!("m{i}"), W, H, &buf),
        ));
    }

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-drag".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        base: base.clone(),
        markers,
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

    // ---- press: enter at START, then BTN_LEFT down ----
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: START.0 as f64,
            y: START.1 as f64,
        })
        .expect("enter");
    let start_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(START.0, START.1, MARKER)
    })
    .expect("marker never composited at drag start");
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("btn down");
    pump_while(&mut queue, &mut app, 5, |a| {
        a.buttons().contains(&(BTN_LEFT, true))
    });
    assert!(
        app.buttons().contains(&(BTN_LEFT, true)),
        "BTN_LEFT press delivered"
    );

    // ---- drag: stream the motions; each composites a marker at its exact cell, in order ----
    let mut prev_serial = start_frame.serial;
    for (i, &(px, py)) in TRACK.iter().enumerate() {
        h.input_tx
            .send(InputCommand::PointerMotion {
                x: px as f64,
                y: py as f64,
            })
            .expect("drag motion");
        let f = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
            f.serial > prev_serial && f.width == W && f.pixel_is(px, py, MARKER)
        })
        .unwrap_or_else(|| panic!("drag motion {i} never re-composited at {:?}", (px, py)));
        assert_eq!(
            f.pixel(px, py).unwrap(),
            MARKER,
            "marker at drag point {:?}",
            (px, py)
        );
        // The previous cell cleared (the marker MOVED across the drag, it did not smear).
        let (ppx, ppy) = if i == 0 { START } else { TRACK[i - 1] };
        assert_eq!(
            f.pixel(ppx, ppy).unwrap(),
            BASE,
            "previous drag cell {:?} cleared",
            (ppx, ppy)
        );
        assert!(
            f.serial > prev_serial,
            "drag frame {i} presents after the previous"
        );
        prev_serial = f.serial;
        save_frame(&format!("pointer_drag_stream-{i}"), &f);
    }

    // ---- release: BTN_LEFT up ----
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .expect("btn up");
    pump_while(&mut queue, &mut app, 5, |a| {
        a.buttons().contains(&(BTN_LEFT, false))
    });

    // ---- exact wire sequence ----
    // Every TRACK motion delivered, IN ORDER (surface-local == root, toplevel roots at (0,0)).
    let motions = app.motions();
    for &pt in TRACK.iter() {
        assert!(
            motions.contains(&pt),
            "motion {pt:?} was delivered, got {motions:?}"
        );
    }
    // The TRACK subsequence appears in the delivered motion order (no reordering).
    let idxs: Vec<usize> = TRACK
        .iter()
        .map(|pt| motions.iter().position(|m| m == pt).unwrap())
        .collect();
    assert!(
        idxs.windows(2).all(|w| w[0] < w[1]),
        "motions arrived in injected order, got {motions:?}"
    );
    // Buttons: exactly a press then a release of BTN_LEFT.
    assert_eq!(
        app.buttons(),
        vec![(BTN_LEFT, true), (BTN_LEFT, false)],
        "press then release"
    );
    // Serials of serial-carrying events (enter, btn-down, btn-up) are strictly monotonic.
    let serials = app.serials();
    assert_eq!(
        serials.len(),
        3,
        "enter + 2 buttons carry serials, got {:?}",
        app.ev
    );
    assert!(
        serials.windows(2).all(|w| w[1] > w[0]),
        "serials strictly increase: {serials:?}"
    );
    // Motion timestamps are non-decreasing (one monotonic input clock).
    let times: Vec<u32> = app
        .ev
        .iter()
        .filter_map(|e| match e {
            PtrEv::Motion(_, _, t) => Some(*t),
            _ => None,
        })
        .collect();
    assert!(
        times.windows(2).all(|w| w[1] >= w[0]),
        "motion timestamps non-decreasing: {times:?}"
    );
    assert_eq!(
        app.redraws as usize,
        1 + TRACK.len(),
        "client redrew once per delivered drag position"
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
                app.surface.attach(Some(&app.base), 0, 0);
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
        qh: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                app.ev.push(PtrEv::Enter(serial));
                app.draw_marker_at(surface_x.round() as i32, surface_y.round() as i32, qh);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                time,
            } => {
                app.ev.push(PtrEv::Motion(
                    surface_x.round() as i32,
                    surface_y.round() as i32,
                    time,
                ));
                app.draw_marker_at(surface_x.round() as i32, surface_y.round() as i32, qh);
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state,
                ..
            } => {
                app.ev.push(PtrEv::Button(
                    button,
                    matches!(state, WEnum::Value(ButtonState::Pressed)),
                    serial,
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
