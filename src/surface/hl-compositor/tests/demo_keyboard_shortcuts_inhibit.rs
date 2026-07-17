//! DEMO — `keyboard_shortcuts_inhibit` (`zwp_keyboard_shortcuts_inhibit_v1` — key-grab).
//!
//! A terminal, an embedded VNC/RDP viewer, or a game asks the compositor to STOP intercepting its own
//! keyboard shortcuts for a surface so ALL keys reach the app. This demo drives a real in-process client
//! that maps a toplevel and creates a shortcuts-inhibitor for its surface + seat, then asserts BOTH real
//! effects the adapter produces: the client receives the `active` wire event (the grant is live) AND the
//! compositor recorded the inhibited surface in its shared observation side-channel (so a real shortcut
//! handler would consult it). Destroying the inhibitor then clears both — proving the adapter genuinely
//! tracks the grab lifecycle, not merely that the global binds.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 128;
const H: i32 = 72;

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    /// The last `active`(true) / `inactive`(false) event the inhibitor delivered.
    inhibit_active: Option<bool>,
}

#[test]
fn keyboard_shortcuts_inhibit_grants_and_tracks() {
    let h = Harness::start("keyboard_shortcuts_inhibit");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    // The newly-wired global under test.
    let ksi_mgr: ZwpKeyboardShortcutsInhibitManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_keyboard_shortcuts_inhibit_manager_v1 advertised");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "ksi",
        W,
        H,
        &solid(W, H, [0x40, 0x40, 0x40, 0xFF]),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-ksi".to_string());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
        inhibit_active: None,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "surface never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    let pid = surface.id().protocol_id();

    // ---- create an inhibitor: the compositor grants it (activate) → client gets `active`, server records it.
    let inhibitor: ZwpKeyboardShortcutsInhibitorV1 =
        ksi_mgr.inhibit_shortcuts(&surface, &seat, &qh, ());
    let granted = poll(&mut queue, &mut app, 5, |app| {
        app.inhibit_active == Some(true)
            && h.observations
                .lock()
                .unwrap()
                .shortcuts_inhibited
                .contains(&pid)
    });
    assert!(
        granted,
        "inhibitor activated: client saw `active`={:?}, server tracked surface {}? {}",
        app.inhibit_active,
        pid,
        h.observations
            .lock()
            .unwrap()
            .shortcuts_inhibited
            .contains(&pid)
    );

    // ---- destroy the inhibitor → the compositor drops it from its tracking (grab released) ----
    inhibitor.destroy();
    let released = poll(&mut queue, &mut app, 5, |_app| {
        !h.observations
            .lock()
            .unwrap()
            .shortcuts_inhibited
            .contains(&pid)
    });
    assert!(
        released,
        "inhibitor destroyed → surface {pid} no longer tracked as key-grabbed"
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
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
impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        e: <ZwpKeyboardShortcutsInhibitorV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => app.inhibit_active = Some(true),
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                app.inhibit_active = Some(false)
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
    XdgToplevel,
    WlSeat,
    ZwpKeyboardShortcutsInhibitManagerV1
);
