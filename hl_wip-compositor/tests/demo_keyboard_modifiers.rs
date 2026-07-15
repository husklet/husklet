//! DEMO 8 — `keyboard_modifiers` (held modifiers reach the client as exact `wl_keyboard.modifiers`).
//!
//! A client maps a toplevel + creates a keyboard and takes focus. The test injects a held modifier and a
//! letter key through the host seam and asserts the client receives, on the WIRE:
//!
//!   * `wl_keyboard.modifiers` with `mods_depressed == 1` (xkb real-modifier index 0 = Shift) while
//!     LEFTSHIFT is held, and the `wl_keyboard.key` for the letter carries the exact evdev keycode;
//!   * `mods_depressed == 0` after the modifier is released;
//!   * `mods_depressed == 4` (index 2 = Control) while LEFTCTRL is held, back to 0 on release.
//!
//! This proves the compositor's xkb modifier state tracking is delivered faithfully — a toolkit reading
//! Shift/Ctrl chords (select-text, Ctrl+C) sees the right state, not a stuck or dropped modifier.

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
const COLOR: [u8; 4] = [0x30, 0x60, 0xC0, 0xFF];
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTCTRL: u32 = 29;
const KEY_A: u32 = 30;
// xkb real-modifier masks (stable across layouts): Shift = index 0 (bit 0), Control = index 2 (bit 2).
const MOD_SHIFT: u32 = 1;
const MOD_CTRL: u32 = 4;

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    /// `wl_keyboard.modifiers` `mods_depressed` values, in arrival order.
    mods: Vec<u32>,
    /// `wl_keyboard.key` `(keycode, pressed)` in arrival order.
    keys: Vec<(u32, bool)>,
}

impl App {
    fn last_mods(&self) -> Option<u32> {
        self.mods.last().copied()
    }
}

#[test]
fn keyboard_modifiers() {
    let h = Harness::start("keyboard_modifiers");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "kbd", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-modifiers".into());
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(), buffer: buffer.clone(), drawn: false, frame_done: false,
        mods: Vec::new(), keys: Vec::new(),
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let mapped = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, COLOR))
        .expect("mapped frame never composited");

    // Focus so keyboard events (and the initial modifiers=0) route to our keyboard object.
    h.input_tx.send(InputCommand::FocusToplevelIndex(0)).expect("focus");
    pump_while(&mut queue, &mut app, 5, |a| !a.mods.is_empty());
    assert_eq!(app.last_mods(), Some(0), "initial modifiers on focus are 0 (nothing held)");

    // ---- hold Shift + press A ----
    h.input_tx.send(InputCommand::Key { keycode: KEY_LEFTSHIFT, pressed: true }).expect("shift down");
    pump_while(&mut queue, &mut app, 5, |a| a.last_mods() == Some(MOD_SHIFT));
    assert_eq!(app.last_mods(), Some(MOD_SHIFT), "mods_depressed == Shift while LEFTSHIFT held");

    h.input_tx.send(InputCommand::Key { keycode: KEY_A, pressed: true }).expect("A down");
    pump_while(&mut queue, &mut app, 5, |a| a.keys.contains(&(KEY_A, true)));
    assert!(app.keys.contains(&(KEY_A, true)), "key event for KEY_A (pressed) delivered while Shift held");
    assert_eq!(app.last_mods(), Some(MOD_SHIFT), "Shift still reported held during the letter key");

    // ---- release A + Shift ----
    h.input_tx.send(InputCommand::Key { keycode: KEY_A, pressed: false }).expect("A up");
    h.input_tx.send(InputCommand::Key { keycode: KEY_LEFTSHIFT, pressed: false }).expect("shift up");
    pump_while(&mut queue, &mut app, 5, |a| a.last_mods() == Some(0));
    assert_eq!(app.last_mods(), Some(0), "mods_depressed back to 0 after Shift release");

    // ---- hold Ctrl ----
    h.input_tx.send(InputCommand::Key { keycode: KEY_LEFTCTRL, pressed: true }).expect("ctrl down");
    pump_while(&mut queue, &mut app, 5, |a| a.last_mods() == Some(MOD_CTRL));
    assert_eq!(app.last_mods(), Some(MOD_CTRL), "mods_depressed == Control while LEFTCTRL held");
    h.input_tx.send(InputCommand::Key { keycode: KEY_LEFTCTRL, pressed: false }).expect("ctrl up");
    pump_while(&mut queue, &mut app, 5, |a| a.last_mods() == Some(0));
    assert_eq!(app.last_mods(), Some(0), "mods_depressed back to 0 after Control release");

    // Exact key log: every injected key — modifier keys included — surfaces as a `wl_keyboard.key`
    // event (Wayland delivers modifier keycodes AND tracks their modifier state), in injection order.
    assert_eq!(
        app.keys,
        vec![
            (KEY_LEFTSHIFT, true), (KEY_A, true), (KEY_A, false), (KEY_LEFTSHIFT, false),
            (KEY_LEFTCTRL, true), (KEY_LEFTCTRL, false),
        ],
        "exact key event sequence (modifier keycodes are delivered as key events too)",
    );

    save_frame("keyboard_modifiers-window", &mapped);

    h.shutdown();
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
impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &WlKeyboard, e: <WlKeyboard as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => app.mods.push(mods_depressed),
            wl_keyboard::Event::Key { key, state, .. } => {
                app.keys.push((key, matches!(state, WEnum::Value(KeyState::Pressed))));
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, XdgToplevel);
