//! DEMO — `key_drives_render` (a key press/release + a modifier changes the client's RENDERED state).
//!
//! `demo_keyboard_modifiers` proves the modifier BITS reach the client; this closes the loop to PIXELS: a
//! client renders a status indicator whose color reflects its live keyboard state, and the test drives
//! that state from OUTSIDE and asserts the composited indicator color changes exactly:
//!
//!   * focus → indicator IDLE (nothing held);
//!   * inject KEY_A down → the client redraws the indicator PRESSED, and the compositor composites PRESSED;
//!   * inject KEY_A up → back to IDLE, composited;
//!   * inject LEFTSHIFT down → the `wl_keyboard.modifiers` change drives the indicator to SHIFTED,
//!     composited; release → IDLE.
//!
//! Proves a key press/release AND a modifier-state change both drive a real client re-render end to end —
//! not just an event log, but new pixels on screen.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, KeyState, WlKeyboard},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const BG: [u8; 4] = [0x20, 0x20, 0x20, 0xFF];
const IDLE: [u8; 4] = [0x60, 0x60, 0x60, 0xFF]; // gray indicator, nothing held
const PRESSED: [u8; 4] = [0x20, 0xE0, 0x40, 0xFF]; // green, a key is down
const SHIFTED: [u8; 4] = [0x40, 0x80, 0xF0, 0xFF]; // blue, shift is held
const IX: i32 = W / 2;
const IY: i32 = H / 2;
const IND: i32 = 40; // indicator box side
const KEY_A: u32 = 30;
const KEY_LEFTSHIFT: u32 = 42;
const MOD_SHIFT: u32 = 1;

/// The client's keyboard-driven render state — the SINGLE source of the indicator color.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Idle,
    Pressed,
    Shifted,
}

fn state_color(s: State) -> [u8; 4] {
    match s {
        State::Idle => IDLE,
        State::Pressed => PRESSED,
        State::Shifted => SHIFTED,
    }
}

struct App {
    surface: WlSurface,
    buffers: Vec<(State, WlBuffer)>,
    drawn: bool,
    frame_done: bool,
    a_held: bool,
    shift_held: bool,
    state: State,
}

impl App {
    fn recompute(&mut self, qh: &QueueHandle<App>) {
        // Shift takes visual precedence over a plain key (a modifier is a distinct rendered mode).
        let next = if self.shift_held {
            State::Shifted
        } else if self.a_held {
            State::Pressed
        } else {
            State::Idle
        };
        if next != self.state {
            self.state = next;
            let buf = &self.buffers.iter().find(|(s, _)| *s == next).unwrap().1;
            self.surface.attach(Some(buf), 0, 0);
            self.surface.damage(0, 0, W, H);
            let _cb: WlCallback = self.surface.frame(qh, ());
            self.surface.commit();
        }
    }
}

fn indicator_buffer(state: State) -> Vec<u8> {
    let mut buf = solid(W, H, BG);
    fill_rect(
        &mut buf,
        W,
        H,
        IX - IND / 2,
        IY - IND / 2,
        IND,
        IND,
        state_color(state),
    );
    buf
}

#[test]
fn key_drives_render() {
    let h = Harness::start("key_drives_render");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let buffers = vec![
        (
            State::Idle,
            make_buffer(
                &shm,
                &qh,
                &h.runtime_dir,
                "idle",
                W,
                H,
                &indicator_buffer(State::Idle),
            ),
        ),
        (
            State::Pressed,
            make_buffer(
                &shm,
                &qh,
                &h.runtime_dir,
                "pressed",
                W,
                H,
                &indicator_buffer(State::Pressed),
            ),
        ),
        (
            State::Shifted,
            make_buffer(
                &shm,
                &qh,
                &h.runtime_dir,
                "shifted",
                W,
                H,
                &indicator_buffer(State::Shifted),
            ),
        ),
    ];

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-key-render".into());
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffers,
        drawn: false,
        frame_done: false,
        a_held: false,
        shift_held: false,
        state: State::Idle,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let idle = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(IX, IY, IDLE)
    })
    .expect("idle indicator never composited");
    assert_eq!(idle.pixel(IX, IY).unwrap(), IDLE, "indicator starts IDLE");

    h.input_tx
        .send(InputCommand::FocusToplevelIndex(0))
        .expect("focus");
    let _ = queue.roundtrip(&mut app);

    // ---- KEY_A down → PRESSED composited ----
    let s0 = idle.serial;
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .expect("A down");
    let pressed = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > s0 && f.pixel_is(IX, IY, PRESSED)
    })
    .expect("PRESSED indicator never composited after KEY_A down");
    assert_eq!(
        pressed.pixel(IX, IY).unwrap(),
        PRESSED,
        "indicator PRESSED while KEY_A held"
    );

    // ---- KEY_A up → IDLE composited ----
    let s1 = pressed.serial;
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_A,
            pressed: false,
        })
        .expect("A up");
    let idle2 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > s1 && f.pixel_is(IX, IY, IDLE)
    })
    .expect("IDLE indicator never re-composited after KEY_A up");
    assert_eq!(
        idle2.pixel(IX, IY).unwrap(),
        IDLE,
        "indicator back to IDLE after KEY_A release"
    );

    // ---- LEFTSHIFT down → SHIFTED composited (a modifier drives the render) ----
    let s2 = idle2.serial;
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_LEFTSHIFT,
            pressed: true,
        })
        .expect("shift down");
    let shifted = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > s2 && f.pixel_is(IX, IY, SHIFTED)
    })
    .expect("SHIFTED indicator never composited after LEFTSHIFT down");
    assert_eq!(
        shifted.pixel(IX, IY).unwrap(),
        SHIFTED,
        "indicator SHIFTED while shift held"
    );
    save_frame("key_drives_render-shifted", &shifted);

    // ---- LEFTSHIFT up → IDLE composited ----
    let s3 = shifted.serial;
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_LEFTSHIFT,
            pressed: false,
        })
        .expect("shift up");
    let idle3 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > s3 && f.pixel_is(IX, IY, IDLE)
    })
    .expect("IDLE indicator never re-composited after LEFTSHIFT up");
    assert_eq!(
        idle3.pixel(IX, IY).unwrap(),
        IDLE,
        "indicator back to IDLE after shift release"
    );

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
                let buf = &app
                    .buffers
                    .iter()
                    .find(|(s, _)| *s == State::Idle)
                    .unwrap()
                    .1;
                app.surface.attach(Some(buf), 0, 0);
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
impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        e: <WlKeyboard as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = matches!(state, WEnum::Value(KeyState::Pressed));
                if key == KEY_A {
                    app.a_held = pressed;
                }
                app.recompute(qh);
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                app.shift_held = mods_depressed & MOD_SHIFT != 0;
                app.recompute(qh);
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
    XdgToplevel
);
