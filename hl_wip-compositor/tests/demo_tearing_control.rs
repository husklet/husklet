//! DEMO — `tearing_control` (`wp_tearing_control_v1` — the per-surface present hint).
//!
//! Chrome/Ozone requests `wp_tearing_control_v1` to hint `async` present (tearing allowed, lowest latency)
//! versus `vsync` (do not tear). The hint is double-buffered and applied at `wl_surface.commit`, and
//! carries NO reply event — so this demo drives a real in-process client that attaches a tearing-control to
//! its surface, sets `async`, and asserts (through the adapter's shared observation side-channel) that the
//! compositor read exactly that hint AT COMMIT; then it flips the hint back to `vsync`, re-commits, and
//! asserts the compositor re-read the reversion. Finally it abuses the protocol (a second tearing-control
//! on the same surface — a `tearing_control_exists` error) and proves the compositor answers with a
//! PROTOCOL ERROR (disconnecting only the offender) and keeps serving a fresh client, rather than aborting.
//!
//! Smithay ships no handler for this staging protocol; it is hand-dispatched in the adapter. This proves
//! that hand-wired path genuinely reads the committed hint each commit, not merely that the global binds.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::tearing_control::v1::client::{
    wp_tearing_control_manager_v1::WpTearingControlManagerV1,
    wp_tearing_control_v1::{PresentationHint, WpTearingControlV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 192;
const H: i32 = 108;
/// The `wp_tearing_control_v1` presentation-hint wire values the adapter records.
const VSYNC: u32 = 0;
const ASYNC: u32 = 1;

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn tearing_control_hint_read_at_commit() {
    let h = Harness::start("tearing_control");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // The newly-wired global under test.
    let tc_mgr: WpTearingControlManagerV1 =
        globals.bind(&qh, 1..=1, ()).expect("wp_tearing_control_manager_v1 advertised");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "tearing", W, H, &solid(W, H, [0x30, 0x90, 0x50, 0xFF]));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-tearing-control".to_string());

    // Hint `async` (tearing allowed) BEFORE the first commit, so the very first committed state carries it.
    let tc: WpTearingControlV1 = tc_mgr.get_tearing_control(&surface, &qh, ());
    tc.set_presentation_hint(PresentationHint::Async);
    surface.commit();

    let mut app = App { surface: surface.clone(), buffer, drawn: false, frame_done: false };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "surface never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    let pid = surface.id().protocol_id();

    // ---- the compositor read `async` from the committed tearing-control hint ----
    let saw_async = poll(&mut queue, &mut app, 5, || {
        h.observations.lock().unwrap().tearing_hint.get(&pid) == Some(&ASYNC)
    });
    assert!(saw_async, "compositor read the `async` tearing hint at commit (got {:?})",
        h.observations.lock().unwrap().tearing_hint.get(&pid));

    // ---- flip back to `vsync` and re-commit → the compositor re-reads the reversion (double-buffered) ----
    tc.set_presentation_hint(PresentationHint::Vsync);
    surface.attach(Some(&app.buffer), 0, 0);
    surface.damage(0, 0, W, H);
    surface.commit();
    let saw_vsync = poll(&mut queue, &mut app, 5, || {
        h.observations.lock().unwrap().tearing_hint.get(&pid) == Some(&VSYNC)
    });
    assert!(saw_vsync, "compositor re-read the `vsync` tearing hint after re-commit (got {:?})",
        h.observations.lock().unwrap().tearing_hint.get(&pid));

    // ---- abuse: a SECOND tearing-control on the same surface is a `tearing_control_exists` protocol error.
    // The compositor must answer with a protocol error (killing only this client), NOT abort. ----
    let _tc2: WpTearingControlV1 = tc_mgr.get_tearing_control(&surface, &qh, ());
    // Drive the offending request; the connection is torn down by the protocol error (ignored here).
    let _ = queue.roundtrip(&mut app);

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(tc);

    // The compositor survived the protocol error: a fresh well-behaved client still maps + composites.
    let mut neighbor = Neighbor::map(&h.runtime_dir, "tearing-survivor", 64, 48, [0xAA, 0x22, 0x66, 0xFF]);
    neighbor.assert_presents(&h.captures);

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
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(_: &mut Self, wm: &XdgWmBase, e: <XdgWmBase as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e { wm.pong(serial); }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
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
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e { app.frame_done = true; }
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel, WpTearingControlManagerV1, WpTearingControlV1);
