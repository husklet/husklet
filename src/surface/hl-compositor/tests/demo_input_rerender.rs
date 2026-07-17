//! DEMO 4 — `input_drives_rerender` (the events-fluency check).
//!
//! A demo client that, on receiving pointer enter/motion, redraws a bright marker at the pointer's
//! surface-local position and commits. The test injects a SEQUENCE of pointer moves and asserts that
//! across successive composited frames the marker MOVES to exactly the injected positions — frame N has
//! the marker at position N and BACKGROUND where the previous marker was. That end-to-end chain (inject
//! over the socket -> client receives motion on the wire -> client redraws -> compositor re-composites ->
//! new pixels captured) proves events drive rendering fluently from OUTSIDE.

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

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x30, 0x30, 0x30, 0xFF]; // dark gray
const MARKER: [u8; 4] = [0x20, 0xE0, 0xE0, 0xFF]; // bright cyan
const M: i32 = 10; // marker box size (centered on the pointer)

// The injected pointer track (surface-local == global; the toplevel roots at (0,0)). First move ENTERS
// the surface (delivered on wl_pointer.enter), the rest are motions. All well inside the surface.
const TRACK: [(i32, i32); 4] = [(40, 30), (150, 30), (150, 110), (40, 110)];

struct App {
    tl_surface: WlSurface,
    base_buffer: WlBuffer,
    markers: Vec<((i32, i32), WlBuffer)>,
    tl_drawn: bool,
    tl_frame_done: bool,
    redraws: u32,
}

impl App {
    fn draw_marker_at(&mut self, sx: i32, sy: i32, qh: &QueueHandle<App>) {
        if let Some((_, buf)) = self.markers.iter().find(|(p, _)| *p == (sx, sy)) {
            self.tl_surface.attach(Some(buf), 0, 0);
            self.tl_surface.damage(0, 0, W, H);
            let _cb: WlCallback = self.tl_surface.frame(qh, ());
            self.tl_surface.commit();
            self.redraws += 1;
        }
    }
}

#[test]
fn input_drives_rerender() {
    let h = Harness::start("input_rerender");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    // One pre-drawn buffer per track position, marker centered there.
    let mut markers = Vec::new();
    for (i, &(px, py)) in TRACK.iter().enumerate() {
        let mut buf = solid(W, H, BASE);
        fill_rect(&mut buf, W, H, px - M / 2, py - M / 2, M, M, MARKER);
        markers.push((
            (px, py),
            make_buffer(&shm, &qh, &h.runtime_dir, &format!("m{i}"), W, H, &buf),
        ));
    }

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-input".into());
    tl_surface.commit();

    let mut app = App {
        tl_surface: tl_surface.clone(),
        base_buffer,
        markers,
        tl_drawn: false,
        tl_frame_done: false,
        redraws: 0,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W
            && f.pixel_is(1, 1, BASE))
        .is_some(),
        "base frame never composited",
    );

    // Create the pointer so injected motion routes to a live client object, then focus + track.
    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _ = queue.roundtrip(&mut app);
    h.input_tx.send(InputCommand::FocusTopmostKeyboard).ok();

    let mut frame_paths = Vec::new();
    for (i, &(px, py)) in TRACK.iter().enumerate() {
        h.input_tx
            .send(InputCommand::PointerMotion {
                x: px as f64,
                y: py as f64,
            })
            .expect("inject motion");

        // Wait for a composited frame whose marker is at THIS position (uniquely identifies this redraw:
        // only this buffer has MARKER at (px,py)).
        let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
            f.width == W && f.height == H && f.pixel_is(px, py, MARKER)
        })
        .unwrap_or_else(|| {
            panic!(
                "marker never re-rendered at injected position {:?}",
                (px, py)
            )
        });

        // Exact pixels: marker center is MARKER, a corner far from it stays BASE, and the PREVIOUS
        // marker cell is now BASE again (the marker MOVED, it did not smear).
        assert_eq!(
            frame.pixel(px, py).unwrap(),
            MARKER,
            "marker present at injected {:?}",
            (px, py)
        );
        assert_eq!(
            frame.pixel(1, 1).unwrap(),
            BASE,
            "background stays BASE away from the marker"
        );
        if i > 0 {
            let (ppx, ppy) = TRACK[i - 1];
            assert_eq!(
                frame.pixel(ppx, ppy).unwrap(),
                BASE,
                "previous marker cell {:?} cleared to BASE",
                (ppx, ppy)
            );
        }

        let name = format!("input_rerender-frame{i}");
        save_frame(&name, &frame);
        frame_paths.push(name);
    }

    // The client re-rendered once per injected move — events drove rendering, not a single static frame.
    assert_eq!(
        app.redraws as usize,
        TRACK.len(),
        "client redrew once per injected pointer move"
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
            if !app.tl_drawn {
                app.tl_surface.attach(Some(&app.base_buffer), 0, 0);
                app.tl_surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.tl_surface.frame(qh, ());
                app.tl_surface.commit();
                app.tl_drawn = true;
            }
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
                surface_x,
                surface_y,
                ..
            } => {
                app.draw_marker_at(surface_x.round() as i32, surface_y.round() as i32, qh);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                app.draw_marker_at(surface_x.round() as i32, surface_y.round() as i32, qh);
            }
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
            app.tl_frame_done = true;
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
