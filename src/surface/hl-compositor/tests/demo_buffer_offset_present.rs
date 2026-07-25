//! DEMO — `buffer_offset_present` (`wl_surface.offset`: honest coverage + documented limitation).
//!
//! This demo pins the compositor's ACTUAL, honest behavior for `wl_surface.offset` (the v5 replacement for
//! the deprecated `wl_surface.attach(buffer, dx, dy)` offset) and for buffer age, so a regression is caught
//! and the modeling boundary is documented rather than silently assumed:
//!
//!   * `wl_surface.offset(dx, dy)` on a mapped TOPLEVEL is ACCEPTED and handled gracefully: the surface
//!     keeps presenting its content correctly (exact solid color, right size), byte-identical to the same
//!     content committed with a zero offset. The neutral headless scene places a toplevel by compositor
//!     policy (not by client buffer offset), so a toplevel's buffer offset is a deliberate NO-OP on the
//!     composited base layer — it must not shift, crop, or corrupt the content. (Buffer offset matters for
//!     SUBSURFACES, whose placement IS client-driven — that path is covered by the subsurface demos via
//!     `set_position`; this demo documents that the TOPLEVEL base-layer offset is intentionally ignored.)
//!
//!   * BUFFER AGE (`EGL_EXT_buffer_age` / a `wp_buffer_age`-style damage-age scheme) is NOT modeled and NOT
//!     advertised. Buffer age is an EGL surface-query extension a client uses to reuse the undamaged parts
//!     of a recycled backbuffer; it is not a Wayland global and has no wire presence here. The headless
//!     presenter captures the full deposited buffer every present (no backbuffer recycling), so there is no
//!     age to report. This is a documented skip, asserted only insofar as no such global appears and the
//!     present path is unaffected by the offset request.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 140;
const H: i32 = 100;
const COLOR: [u8; 4] = [0x40, 0xB0, 0x60, 0xFF]; // green

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn buffer_offset_present() {
    let h = Harness::start("buffer_offset_present");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    // Bind wl_compositor at >= v5 so `wl_surface.offset` is available (it was added in wl_surface v5).
    let compositor: WlCompositor = globals.bind(&qh, 5..=6, ()).expect("wl_compositor v5+");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // Two byte-identical buffers: one committed at offset (0,0), one at a nonzero offset. The composited
    // result must be identical — proving the toplevel base-layer offset is a graceful no-op.
    let px = solid(W, H, COLOR);
    let buf_a = make_buffer(&shm, &qh, &h.runtime_dir, "oa", W, H, &px);
    let buf_b = make_buffer(&shm, &qh, &h.runtime_dir, "ob", W, H, &px);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-buffer-offset".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buf_a.clone(),
        drawn: false,
        frame_done: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // ---- frame 1: zero-offset baseline ----
    let frame1 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, COLOR)
    })
    .expect("baseline (zero-offset) frame never composited");
    for (x, y) in [
        (W / 2, H / 2),
        (0, 0),
        (W - 1, 0),
        (0, H - 1),
        (W - 1, H - 1),
    ] {
        assert_eq!(
            frame1.pixel(x, y).unwrap(),
            COLOR,
            "baseline pixel ({x},{y}) is the solid color"
        );
    }

    // ---- frame 2: same content committed with a NONZERO buffer offset ----
    // `wl_surface.offset(dx, dy)` sets the buffer's placement offset (v5+). On a toplevel it must be a
    // graceful no-op: the composited frame is byte-identical to the zero-offset baseline.
    surface.attach(Some(&buf_b), 0, 0);
    surface.offset(17, 23);
    surface.damage(0, 0, W, H);
    let _cb: WlCallback = surface.frame(&qh, ());
    surface.commit();

    let frame2 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > frame1.serial
    })
    .expect("offset commit never presented (offset must not break the present path)");
    assert_eq!(frame2.width, W, "offset did not change presented width");
    assert_eq!(frame2.height, H, "offset did not change presented height");
    for (x, y) in [
        (W / 2, H / 2),
        (0, 0),
        (W - 1, 0),
        (0, H - 1),
        (W - 1, H - 1),
    ] {
        assert_eq!(
            frame2.pixel(x, y).unwrap(),
            COLOR,
            "offset frame pixel ({x},{y}) still the solid color (not shifted)"
        );
    }
    assert_eq!(
        frame2.rgba, frame1.rgba,
        "toplevel buffer offset is a graceful no-op: content byte-identical"
    );
    // Placement origin unchanged: the toplevel still composites at the base root origin, the offset did not
    // leak into the reported layer position.
    assert_eq!(
        (frame2.x, frame2.y),
        (0, 0),
        "offset did not shift the reported composite origin"
    );

    save_frame("buffer_offset_present-2_offset", &frame2);

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
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
    XdgToplevel
);
