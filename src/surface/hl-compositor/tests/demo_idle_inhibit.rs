//! DEMO — `idle_inhibit` (`zwp_idle_inhibit_manager_v1` — the screensaver / DPMS inhibitor).
//!
//! A video player or presentation tool binds `zwp_idle_inhibit_manager_v1` and creates a
//! `zwp_idle_inhibitor_v1` on its surface to keep the system awake while it plays. The protocol carries NO
//! reply event — the compositor simply TRACKS the inhibitor — so this demo drives a real in-process client
//! that maps a toplevel, creates an inhibitor on it, and asserts (through the adapter's shared observation
//! side-channel) that the compositor registered exactly that surface; then it destroys the inhibitor and
//! asserts the compositor dropped it.
//!
//! Proves the adapter's newly-wired idle-inhibit global genuinely tracks an inhibitor's whole lifecycle
//! (create → registered on the exact surface, destroy → unregistered), not merely that the global binds.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 90;

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn idle_inhibit_tracked_and_removed() {
    let h = Harness::start("idle_inhibit");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // The newly-wired global under test.
    let idle_mgr: ZwpIdleInhibitManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_idle_inhibit_manager_v1 advertised");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "player",
        W,
        H,
        &solid(W, H, [0x30, 0xC0, 0x50, 0xFF]),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-idle-inhibit".to_string());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "player never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // The shared key both ends agree on: the wl_surface's client-assigned protocol id.
    let pid = surface.id().protocol_id();

    // Before any inhibitor exists, the surface must NOT be tracked as inhibiting.
    let _ = queue.roundtrip(&mut app);
    assert!(
        !h.observations.lock().unwrap().idle_inhibited.contains(&pid),
        "no inhibitor created yet, surface must not be inhibiting"
    );

    // ---- create the inhibitor on the surface → compositor tracks it ----
    let inhibitor: ZwpIdleInhibitorV1 = idle_mgr.create_inhibitor(&surface, &qh, ());
    let tracked = poll(&mut queue, &mut app, 5, |a| {
        let _ = a;
        h.observations.lock().unwrap().idle_inhibited.contains(&pid)
    });
    assert!(
        tracked,
        "compositor registered an idle inhibitor for the surface after create_inhibitor"
    );
    // EXACT set membership: the inhibited set is precisely {this surface}, nothing spurious.
    {
        let obs = h.observations.lock().unwrap();
        assert_eq!(
            obs.idle_inhibited.iter().copied().collect::<Vec<_>>(),
            vec![pid],
            "exactly one inhibited surface (ours), no others"
        );
    }

    // ---- destroy the inhibitor → compositor drops it ----
    inhibitor.destroy();
    let removed = poll(&mut queue, &mut app, 5, |a| {
        let _ = a;
        !h.observations.lock().unwrap().idle_inhibited.contains(&pid)
    });
    assert!(
        removed,
        "compositor unregistered the inhibitor after zwp_idle_inhibitor_v1.destroy"
    );
    assert!(
        h.observations.lock().unwrap().idle_inhibited.is_empty(),
        "no inhibited surfaces remain after destroy"
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    h.shutdown();
}

/// Pump the client queue until `pred` holds (server-side state settled) or `secs` elapse.
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
    XdgToplevel,
    ZwpIdleInhibitManagerV1,
    ZwpIdleInhibitorV1
);
