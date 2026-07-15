//! DEMO 6 — `multi_window_keyboard_focus` (focus follows the active window; keys go only to it).
//!
//! TWO independent Wayland clients (separate `Connection`s — separate compositor clients) each map a
//! toplevel in a distinct color. The test moves keyboard focus A → B through the host seam and asserts,
//! on the WIRE, the exact `wl_keyboard` focus protocol:
//!
//!   * focus A  →  A receives `wl_keyboard.enter` (naming A's surface), B receives nothing.
//!   * focus B  →  A receives `wl_keyboard.leave` (its enter is now balanced by a leave), B receives
//!                 `wl_keyboard.enter`.
//!   * inject a key while B holds focus  →  ONLY B receives `wl_keyboard.key` (evdev keycode + pressed);
//!                 A's key log stays empty.
//!
//! Ordering is asserted, not just presence: A's event log is exactly `[Enter, Leave]` and B's is
//! `[Enter, Key]`. Both toplevels' pixels are captured and composited into one PNG (each at a
//! test-chosen on-screen slot — the neutral scene roots every toplevel at (0,0), so global placement is
//! the viewer's, asserted per-surface by color).

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_keyboard::{self, KeyState, WlKeyboard}, wl_registry::WlRegistry, wl_seat::WlSeat,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const RED: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF]; // client A
const GREEN: [u8; 4] = [0x20, 0xE0, 0x20, 0xFF]; // client B
const KEY_A: u32 = 30; // evdev KEY_A

/// One `wl_keyboard` event, recorded in arrival order so focus transitions can be asserted exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
enum KbdEv {
    Enter,
    Leave,
    Key(u32, bool),
}

struct Client {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    kbd: Vec<KbdEv>,
}

impl Client {
    fn keys(&self) -> Vec<(u32, bool)> {
        self.kbd.iter().filter_map(|e| match e { KbdEv::Key(k, p) => Some((*k, *p)), _ => None }).collect()
    }
}

/// Stand up one client on the shared socket: bind globals, map a toplevel of `color`, create its
/// keyboard, and drive the map handshake to completion. Returns the connection, queue, app, and the
/// bound `wl_seat` (kept so the caller can create input objects if needed).
fn spawn_client(dir: &std::path::Path, tag: &str, color: [u8; 4]) -> (Connection, wayland_client::EventQueue<Client>, Client) {
    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<Client>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let buffer = make_buffer(&shm, &qh, dir, tag, W, H, &solid(W, H, color));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("demo-{tag}"));
    // Create the keyboard up front so focus enter/leave route to a live object.
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = Client { surface: surface.clone(), buffer, drawn: false, frame_done: false, kbd: Vec::new() };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "client {tag} never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = queue.roundtrip(&mut app);
    // Leak the bound objects that must stay alive for the test's duration.
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(_kbd);
    std::mem::forget(seat);
    (conn, queue, app)
}

#[test]
fn multi_window_keyboard_focus() {
    let h = Harness::start("multi_window_kbd");

    // Map A first (its surface id is minted first → index 0), then B (index 1).
    let (_conn_a, mut qa, mut a) = spawn_client(&h.runtime_dir, "A", RED);
    let (_conn_b, mut qb, mut b) = spawn_client(&h.runtime_dir, "B", GREEN);

    // Both toplevels composited their own color (per-surface; scene roots both at (0,0)).
    let fa = wait_for(&h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, RED))
        .expect("client A frame (red) never composited");
    let fb = wait_for(&h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, GREEN))
        .expect("client B frame (green) never composited");
    assert_eq!(fa.pixel(W / 2, H / 2).unwrap(), RED, "A is solid red");
    assert_eq!(fb.pixel(W / 2, H / 2).unwrap(), GREEN, "B is solid green");

    // ---- focus A: A enters, B silent ----
    h.input_tx.send(InputCommand::FocusToplevelIndex(0)).expect("focus A");
    pump2(&mut qa, &mut a, &mut qb, &mut b, 5, |a, _b| a.kbd == [KbdEv::Enter]);
    assert_eq!(a.kbd, [KbdEv::Enter], "A received exactly wl_keyboard.enter on focus A");
    assert!(b.kbd.is_empty(), "B received nothing while A was focused, got {:?}", b.kbd);

    // ---- focus B: A leaves, B enters ----
    h.input_tx.send(InputCommand::FocusToplevelIndex(1)).expect("focus B");
    pump2(&mut qa, &mut a, &mut qb, &mut b, 5, |a, b| a.kbd == [KbdEv::Enter, KbdEv::Leave] && b.kbd == [KbdEv::Enter]);
    assert_eq!(a.kbd, [KbdEv::Enter, KbdEv::Leave], "A got enter THEN leave (focus moved off it)");
    assert_eq!(b.kbd, [KbdEv::Enter], "B got exactly enter on focus B");

    // ---- key while B focused: only B ----
    h.input_tx.send(InputCommand::Key { keycode: KEY_A, pressed: true }).expect("inject key");
    pump2(&mut qa, &mut a, &mut qb, &mut b, 5, |_a, b| b.kbd.contains(&KbdEv::Key(KEY_A, true)));
    assert_eq!(b.keys(), vec![(KEY_A, true)], "B (focused) received the key");
    assert!(a.keys().is_empty(), "A (unfocused) received NO key, got {:?}", a.keys());
    assert_eq!(a.kbd, [KbdEv::Enter, KbdEv::Leave], "A's log is unchanged by the key (still enter,leave)");
    assert_eq!(b.kbd, [KbdEv::Enter, KbdEv::Key(KEY_A, true)], "B's log is exactly enter,key");

    // ---- viewer PNG: both windows side by side ----
    save_composited("multi_window_kbd", 2 * W + 20, H, [0x10, 0x10, 0x10, 0xFF], &[(&fa, 0, 0), (&fb, W + 20, 0)]);

    h.shutdown();
}

/// Roundtrip BOTH client queues while polling a joint predicate, until it holds or `secs` elapse.
fn pump2(
    qa: &mut wayland_client::EventQueue<Client>, a: &mut Client,
    qb: &mut wayland_client::EventQueue<Client>, b: &mut Client,
    secs: u64, done: impl Fn(&Client, &Client) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !done(a, b) {
        assert!(Instant::now() < deadline, "pump2 timed out: A={:?} B={:?}", a.kbd, b.kbd);
        let _ = qa.roundtrip(a);
        let _ = qb.roundtrip(b);
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlRegistry, GlobalListContents> for Client {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<XdgWmBase, ()> for Client {
    fn event(_: &mut Self, wm: &XdgWmBase, e: <XdgWmBase as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e { wm.pong(serial); }
    }
}
impl Dispatch<XdgSurface, ()> for Client {
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
impl Dispatch<WlCallback, ()> for Client {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e { app.frame_done = true; }
    }
}
impl Dispatch<WlKeyboard, ()> for Client {
    fn event(app: &mut Self, _: &WlKeyboard, e: <WlKeyboard as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_keyboard::Event::Enter { surface, .. } if surface.id() == app.surface.id() => app.kbd.push(KbdEv::Enter),
            wl_keyboard::Event::Leave { surface, .. } if surface.id() == app.surface.id() => app.kbd.push(KbdEv::Leave),
            wl_keyboard::Event::Key { key, state, .. } => {
                app.kbd.push(KbdEv::Key(key, matches!(state, WEnum::Value(KeyState::Pressed))));
            }
            _ => {}
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Client {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, XdgToplevel);
