//! DEMO — `content_type` (`wp_content_type_v1` — the per-surface content-type hint).
//!
//! A media/browser client tags its surface with a content-type hint (`photo`/`video`/`game`) so the
//! compositor can pick a tearing / scaling / latency policy. The hint is double-buffered and applied at
//! commit, and carries NO reply event — so this demo drives a real in-process client that maps a toplevel
//! tagged `video`, and asserts (through the adapter's shared observation side-channel) that the compositor
//! read exactly that hint at commit; then it re-tags the surface `game`, re-commits, and asserts the
//! compositor re-read the new hint.
//!
//! Proves the adapter's newly-wired content-type global genuinely reads the committed per-surface hint each
//! commit (and tracks changes), not merely that the global binds.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::content_type::v1::client::{
    wp_content_type_manager_v1::WpContentTypeManagerV1,
    wp_content_type_v1::{self, WpContentTypeV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 192;
const H: i32 = 108;
/// The `wp_content_type_v1.type` wire values (mirrored from the protocol enum) the adapter records.
const VIDEO: u32 = 2;
const GAME: u32 = 3;

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn content_type_hint_read_at_commit() {
    let h = Harness::start("content_type");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // The newly-wired global under test.
    let ct_mgr: WpContentTypeManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("wp_content_type_manager_v1 advertised");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "media",
        W,
        H,
        &solid(W, H, [0x80, 0x40, 0xC0, 0xFF]),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-content-type".to_string());

    // Tag the surface `video` BEFORE the first commit, so the very first committed state carries the hint.
    let ct: WpContentTypeV1 = ct_mgr.get_surface_content_type(&surface, &qh, ());
    ct.set_content_type(wp_content_type_v1::Type::Video);
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "media surface never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    let pid = surface.id().protocol_id();

    // ---- the compositor read `video` from the committed content-type hint ----
    let saw_video = poll(&mut queue, &mut app, 5, || {
        h.observations.lock().unwrap().content_type.get(&pid) == Some(&VIDEO)
    });
    assert!(
        saw_video,
        "compositor read the `video` content-type hint at commit (got {:?})",
        h.observations.lock().unwrap().content_type.get(&pid)
    );

    // ---- re-tag `game` and re-commit → the compositor re-reads the new hint (double-buffered) ----
    ct.set_content_type(wp_content_type_v1::Type::Game);
    surface.attach(Some(&app.buffer), 0, 0);
    surface.damage(0, 0, W, H);
    surface.commit();
    let saw_game = poll(&mut queue, &mut app, 5, || {
        h.observations.lock().unwrap().content_type.get(&pid) == Some(&GAME)
    });
    assert!(
        saw_game,
        "compositor re-read the `game` content-type hint after re-commit (got {:?})",
        h.observations.lock().unwrap().content_type.get(&pid)
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(ct);
    h.shutdown();
}

/// Pump the client queue until `pred` holds (server-side state settled) or `secs` elapse.
fn poll(queue: &mut EventQueue<App>, app: &mut App, secs: u64, pred: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let _ = queue.roundtrip(app);
        if pred() {
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
    WpContentTypeManagerV1,
    WpContentTypeV1
);
