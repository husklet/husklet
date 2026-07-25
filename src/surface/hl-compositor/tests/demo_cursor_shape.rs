//! DEMO — `cursor_shape` (`wp_cursor_shape_device_v1` — named cursors).
//!
//! Chrome/Ozone and modern GTK/Qt set the pointer cursor by SHAPE NAME (`pointer`/`text`/…) through
//! `wp_cursor_shape_device_v1.set_shape` instead of attaching a pixel buffer. The compositor may only honour
//! `set_shape` when the client owns pointer focus (it must present a valid `wl_pointer.enter` serial), so
//! this demo maps a toplevel, drives a real pointer ENTER over it (capturing the enter serial), then creates
//! a cursor-shape device and requests the `pointer` shape with that serial. It asserts the compositor decoded
//! the shape and routed it to the seat — recorded (through the shared observation side-channel) as the exact
//! CSS name `pointer` — proving the adapter's newly-wired cursor-shape global genuinely applies the named
//! cursor, not merely that the global binds. It also proves a STALE serial is rejected (no shape recorded).

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
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const BG: [u8; 4] = [0x28, 0x28, 0x28, 0xFF];
/// A point inside the toplevel to drive the pointer enter to.
const IN: (i32, i32) = (70, 55);

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    /// The serial of the last `wl_pointer.enter` naming our surface — the token `set_shape` must present.
    enter_serial: Option<u32>,
}

#[test]
fn cursor_shape_named_reaches_seat() {
    let h = Harness::start("cursor_shape");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    // The newly-wired global under test.
    let shape_mgr: WpCursorShapeManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .expect("wp_cursor_shape_manager_v1 advertised");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "cs", W, H, &solid(W, H, BG));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-cursor-shape".to_string());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
        enter_serial: None,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W
            && f.pixel_is(1, 1, BG))
        .is_some(),
        "base frame never composited",
    );

    // A pointer + a device on it. The device is inert until the pointer has focus + a valid enter serial.
    let pointer: WlPointer = seat.get_pointer(&qh, ());
    let device: WpCursorShapeDeviceV1 = shape_mgr.get_pointer(&pointer, &qh, ());
    let _ = queue.roundtrip(&mut app);

    // Drive a real ENTER over the surface so the compositor hands the client a valid enter serial.
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: IN.0 as f64,
            y: IN.1 as f64,
        })
        .expect("motion in");
    let got_serial = poll(&mut queue, &mut app, 5, |app| app.enter_serial.is_some());
    assert!(
        got_serial,
        "client never received a wl_pointer.enter serial for its surface"
    );
    let serial = app.enter_serial.unwrap();

    // ---- a STALE serial is REJECTED: no named shape is recorded ----
    device.set_shape(serial.wrapping_sub(1), Shape::Text);
    let _ = queue.roundtrip(&mut app);
    std::thread::sleep(Duration::from_millis(50));
    let _ = queue.roundtrip(&mut app);
    assert!(
        h.observations.lock().unwrap().cursor_shape.is_none(),
        "a stale serial must not set any cursor shape (got {:?})",
        h.observations.lock().unwrap().cursor_shape
    );

    // ---- the VALID enter serial: the compositor decodes `pointer` and routes it to the seat ----
    device.set_shape(serial, Shape::Pointer);
    let named = poll(&mut queue, &mut app, 5, |_app| {
        h.observations.lock().unwrap().cursor_shape.as_deref() == Some("pointer")
    });
    assert!(
        named,
        "compositor recorded the named cursor `pointer` from set_shape (got {:?})",
        h.observations.lock().unwrap().cursor_shape
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(pointer);
    std::mem::forget(device);
    h.shutdown();
}

/// Pump the client queue until `pred(app)` holds (server-side state + client events settled) or `secs` pass.
fn poll(
    queue: &mut EventQueue<App>,
    app: &mut App,
    secs: u64,
    pred: impl Fn(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let _ = queue.roundtrip(app);
        if pred(app) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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
        if let wl_pointer::Event::Enter {
            serial, surface, ..
        } = e
        {
            if surface.id() == app.surface.id() {
                app.enter_serial = Some(serial);
            }
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
    WpCursorShapeManagerV1,
    WpCursorShapeDeviceV1
);
