//! DEMO — `toplevel_close` (the compositor asks a mapped toplevel to close; the client receives it).
//!
//! The compositor-initiated close request Chrome/GTK need for a working window-manager "close" affordance:
//! a client maps a toplevel and composites its first frame, then the host drives
//! [`InputCommand::CloseTopmostToplevel`] (the seam a WM close button / `wm_close` would use). The
//! compositor sends `xdg_toplevel.close`; the client asserts it received EXACTLY that event on its own
//! toplevel, and (proving the teardown path) destroys the toplevel + surface in response and the compositor
//! survives — a well-behaved neighbor still maps + composites afterwards.
//!
//! Before this was wired the headless adapter emitted no `xdg_toplevel.close` at all (there was no host
//! seam), so a toolkit's programmatic/WM-driven close never fired — nothing to assert. Now it does.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const COLOR: [u8; 4] = [0x40, 0x80, 0xC0, 0xFF];

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    close_received: u32,
}

#[test]
fn toplevel_close() {
    let h = Harness::start("toplevel_close");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "close",
        W,
        H,
        &solid(W, H, COLOR),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-close".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
        close_received: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W
            && f.pixel_is(1, 1, COLOR))
        .is_some(),
        "toplevel first frame never composited",
    );

    // ---- the compositor asks the toplevel to close ----
    h.input_tx
        .send(InputCommand::CloseTopmostToplevel)
        .expect("send close");

    let deadline = Instant::now() + Duration::from_secs(5);
    while app.close_received == 0 {
        assert!(
            Instant::now() < deadline,
            "client never received xdg_toplevel.close"
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.close_received, 1,
        "the client received EXACTLY one xdg_toplevel.close"
    );

    // ---- teardown in response: destroy toplevel + surface; the compositor must survive ----
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    let _ = queue.roundtrip(&mut app);

    // A fresh well-behaved neighbor still maps + composites an exact frame — the adapter survived the close.
    let mut neighbor = Neighbor::map(
        &h.runtime_dir,
        "after-close",
        100,
        80,
        [0x20, 0xE0, 0x20, 0xFF],
    );
    neighbor.assert_presents(&h.captures);

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
impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &XdgToplevel,
        e: <XdgToplevel as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = e {
            app.close_received += 1;
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer);
