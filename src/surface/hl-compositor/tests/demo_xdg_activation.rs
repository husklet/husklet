//! DEMO — `xdg_activation` (`xdg_activation_v1` — cross-client activation / focus request).
//!
//! A launcher hands a launched client an activation token; the client calls
//! `xdg_activation_v1.activate(token, surface)` to ask the compositor to bring that surface to the front.
//! The headless single-window policy honours an activation by giving the target toplevel keyboard focus —
//! which the client observes as a `wl_keyboard.enter`. This demo drives ONE real in-process client that maps
//! TWO toplevels (A, B) and binds a `wl_keyboard`, then: (1) mints a token and activates A, asserting the
//! keyboard focus lands EXACTLY on A; (2) mints a second token and activates B, asserting focus MOVES —
//! `wl_keyboard.leave(A)` then `wl_keyboard.enter(B)`.
//!
//! Proves the adapter's newly-wired activation global mints a real token (the `done` event) and HONORS the
//! activation by re-targeting focus onto the exact surface the token was redeemed against — a genuine
//! activation-driven focus change, not merely that the global binds.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 120;
const H: i32 = 80;

struct App {
    surfaces: [WlSurface; 2],
    buffers: [WlBuffer; 2],
    drawn: [bool; 2],
    frame_done: [bool; 2],
    /// The most recent `xdg_activation_token_v1.done` token string (cleared before each mint).
    last_token: Option<String>,
    /// wl_surface protocol ids the keyboard `enter`ed / `leave`d, in order.
    enter_log: Vec<u32>,
    leave_log: Vec<u32>,
    /// The currently keyboard-focused surface protocol id (per enter/leave).
    focused: Option<u32>,
}

#[test]
fn activation_focuses_target_toplevel() {
    let h = Harness::start("xdg_activation");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    // The newly-wired global under test.
    let activation: XdgActivationV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("xdg_activation_v1 advertised");

    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());

    // Map two toplevels A and B.
    let mut surfaces = Vec::new();
    let mut buffers = Vec::new();
    let mut xdgs = Vec::new();
    let mut toplevels = Vec::new();
    for (i, color) in [[0xC0, 0x40, 0x40, 0xFF], [0x40, 0x40, 0xC0, 0xFF]]
        .into_iter()
        .enumerate()
    {
        let buffer = make_buffer(
            &shm,
            &qh,
            &h.runtime_dir,
            &format!("win{i}"),
            W,
            H,
            &solid(W, H, color),
        );
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, i);
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(format!("demo-activation-{i}"));
        surface.commit();
        surfaces.push(surface);
        buffers.push(buffer);
        xdgs.push(xdg);
        toplevels.push(toplevel);
    }

    let mut app = App {
        surfaces: [surfaces[0].clone(), surfaces[1].clone()],
        buffers: [buffers[0].clone(), buffers[1].clone()],
        drawn: [false; 2],
        frame_done: [false; 2],
        last_token: None,
        enter_log: Vec::new(),
        leave_log: Vec::new(),
        focused: None,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn.iter().all(|&d| d) && app.frame_done.iter().all(|&f| f)) {
        assert!(Instant::now() < deadline, "both toplevels never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let pid_a = app.surfaces[0].id().protocol_id();
    let pid_b = app.surfaces[1].id().protocol_id();

    // Nothing is focused before any activation (headless does not auto-focus a mapped toplevel).
    let _ = queue.roundtrip(&mut app);
    assert_eq!(app.focused, None, "no keyboard focus before activation");

    let sa = app.surfaces[0].clone();
    let sb = app.surfaces[1].clone();

    // ---- (1) activate A → keyboard focus lands on A ----
    let token1 = mint_token(&mut queue, &mut app, &activation, &qh, &sa);
    activation.activate(token1, &sa);
    let got_a = poll(&mut queue, &mut app, 5, |a| a.focused == Some(pid_a));
    assert!(
        got_a,
        "activation of A focused A; enter_log={:?}",
        app.enter_log
    );
    assert_eq!(
        app.enter_log.last(),
        Some(&pid_a),
        "the enter targeted EXACTLY surface A"
    );
    assert!(
        !app.enter_log.contains(&pid_b),
        "B was never focused by A's activation"
    );

    // ---- (2) activate B → focus MOVES: leave A, enter B ----
    let token2 = mint_token(&mut queue, &mut app, &activation, &qh, &sb);
    activation.activate(token2, &sb);
    let got_b = poll(&mut queue, &mut app, 5, |a| a.focused == Some(pid_b));
    assert!(
        got_b,
        "activation of B focused B; enter_log={:?} leave_log={:?}",
        app.enter_log, app.leave_log
    );
    assert!(
        app.leave_log.contains(&pid_a),
        "A received wl_keyboard.leave when B was activated"
    );
    assert_eq!(
        app.enter_log.last(),
        Some(&pid_b),
        "the last enter targeted EXACTLY surface B"
    );

    for t in toplevels {
        std::mem::forget(t);
    }
    for x in xdgs {
        std::mem::forget(x);
    }
    std::mem::forget(_kbd);
    h.shutdown();
}

/// Mint one activation token: create an `xdg_activation_token_v1`, name the requesting `surface`, commit,
/// and pump until the compositor answers `done(token)`. Returns the token string to redeem via `activate`.
fn mint_token(
    queue: &mut EventQueue<App>,
    app: &mut App,
    activation: &XdgActivationV1,
    qh: &QueueHandle<App>,
    surface: &WlSurface,
) -> String {
    app.last_token = None;
    let token: XdgActivationTokenV1 = activation.get_activation_token(qh, ());
    token.set_surface(surface);
    token.commit();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.last_token.is_none() {
        assert!(
            Instant::now() < deadline,
            "compositor never answered xdg_activation_token_v1.done"
        );
        let _ = queue.roundtrip(app);
        std::thread::sleep(Duration::from_millis(5));
    }
    std::mem::forget(token);
    app.last_token.take().unwrap()
}

/// Pump the client queue until `pred` holds or `secs` elapse.
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
impl Dispatch<XdgSurface, usize> for App {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        idx: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.drawn[*idx] {
                app.surfaces[*idx].attach(Some(&app.buffers[*idx]), 0, 0);
                app.surfaces[*idx].damage(0, 0, W, H);
                let _cb: WlCallback = app.surfaces[*idx].frame(qh, *idx);
                app.surfaces[*idx].commit();
                app.drawn[*idx] = true;
            }
        }
    }
}
impl Dispatch<WlCallback, usize> for App {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        e: <WlCallback as Proxy>::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.frame_done[*idx] = true;
        }
    }
}
impl Dispatch<XdgActivationTokenV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &XdgActivationTokenV1,
        e: <XdgActivationTokenV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = e {
            app.last_token = Some(token);
        }
    }
}
impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        e: <WlKeyboard as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Enter { surface, .. } => {
                let pid = surface.id().protocol_id();
                app.enter_log.push(pid);
                app.focused = Some(pid);
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                let pid = surface.id().protocol_id();
                app.leave_log.push(pid);
                if app.focused == Some(pid) {
                    app.focused = None;
                }
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
    XdgToplevel,
    XdgActivationV1
);
