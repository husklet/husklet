//! DEMO (batch-3) — `input_latency_ordering` (a burst of input arrives in EXACT order; render is prompt).
//!
//! A mapped toplevel with a `wl_pointer` + `wl_keyboard`. The test injects a BURST of input over the host
//! seam — keyboard focus, a pointer move (enter + motion), a key press/release, a button press/release —
//! and asserts:
//!
//!   * the client receives those events in the EXACT injected order (a fixed expected kind-sequence);
//!   * every serial-bearing event carries a STRICTLY INCREASING serial (monotonic wire serials);
//!   * the pointer-enter redraw (the marker the client paints at the pointer position) reaches the screen
//!     at the VERY NEXT present cycle — the marker frame's present serial is exactly `baseline + 1`, and
//!     no other present happened after the baseline (input → render is prompt and bounded to one cycle).
//!
//! End to end this proves events drive rendering FLUENTLY from outside: injected order is preserved on the
//! wire, and the render they trigger lands in the immediately following frame.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
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

const W: i32 = 200;
const H: i32 = 150;
const BASE: [u8; 4] = [0x28, 0x28, 0x30, 0xFF]; // dark
const MARKER: [u8; 4] = [0xF0, 0xC0, 0x20, 0xFF]; // amber
const MK: i32 = 12; // marker box size
const PX: i32 = 100; // injected pointer position (surface-local == root; toplevel roots at 0,0)
const PY: i32 = 70;
const KEY_A: u32 = 30; // evdev KEY_A
const BTN_LEFT: u32 = 0x110;

/// One recorded input event's KIND (serial elided) — the sequence we assert order on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvKind {
    KbdEnter,
    PtrEnter,
    PtrMotion,
    Key(u32, bool),    // (keycode, pressed)
    Button(u32, bool), // (button, pressed)
}

struct App {
    tl_surface: WlSurface,
    base_buffer: WlBuffer,
    marker_buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
    /// Ordered recorded events (kind + optional wire serial).
    events: Vec<(EvKind, Option<u32>)>,
    /// The marker has been drawn (on the first pointer enter).
    marker_drawn: bool,
}

impl App {
    fn draw_marker(&mut self, qh: &QueueHandle<App>) {
        if self.marker_drawn {
            return;
        }
        self.tl_surface.attach(Some(&self.marker_buffer), 0, 0);
        self.tl_surface.damage(0, 0, W, H);
        let _cb: WlCallback = self.tl_surface.frame(qh, ());
        self.tl_surface.commit();
        self.marker_drawn = true;
    }
}

#[test]
fn input_latency_ordering() {
    let h = Harness::start("input_latency_ordering");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let mut marker_px = solid(W, H, BASE);
    fill_rect(
        &mut marker_px,
        W,
        H,
        PX - MK / 2,
        PY - MK / 2,
        MK,
        MK,
        MARKER,
    );
    let marker_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "marker", W, H, &marker_px);

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-latency".into());
    tl_surface.commit();

    let mut app = App {
        tl_surface: tl_surface.clone(),
        base_buffer: base_buffer.clone(),
        marker_buffer: marker_buffer.clone(),
        tl_drawn: false,
        tl_frame_done: false,
        events: Vec::new(),
        marker_drawn: false,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let base_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, BASE)
    })
    .expect("base frame never composited");
    let baseline = base_frame.serial;

    // Create pointer + keyboard so injected input routes to live client objects.
    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _keyboard: WlKeyboard = seat.get_keyboard(&qh, ());
    let _ = queue.roundtrip(&mut app);

    // Inject the burst — the wire order the client MUST observe is fixed by this send order.
    h.input_tx
        .send(InputCommand::FocusTopmostKeyboard)
        .expect("focus");
    // First motion ENTERS the surface (the enter event conveys the position — no separate motion event);
    // it triggers the marker redraw. A second motion (same surface) then emits a real wl_pointer.motion.
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: PX as f64,
            y: PY as f64,
        })
        .expect("enter");
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: (PX + 20) as f64,
            y: PY as f64,
        })
        .expect("motion");
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .expect("key down");
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_A,
            pressed: false,
        })
        .expect("key up");
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("btn down");
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .expect("btn up");

    // Pump until all 7 events are in AND the marker frame has been captured.
    let expected: Vec<EvKind> = vec![
        EvKind::KbdEnter,
        EvKind::PtrEnter,
        EvKind::PtrMotion,
        EvKind::Key(KEY_A, true),
        EvKind::Key(KEY_A, false),
        EvKind::Button(BTN_LEFT, true),
        EvKind::Button(BTN_LEFT, false),
    ];
    let want = expected.len();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut marker_frame = None;
    loop {
        let _ = queue.roundtrip(&mut app);
        if marker_frame.is_none() {
            marker_frame = h
                .captures
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|f| f.width == W && f.pixel_is(PX, PY, MARKER))
                .cloned();
        }
        if app.events.len() >= want && marker_frame.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "burst incomplete: events={:?} marker={}",
            app.events,
            marker_frame.is_some()
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // ---- (a) EXACT injected order ----
    let got_kinds: Vec<EvKind> = app.events.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        got_kinds, expected,
        "client received the burst in the exact injected order"
    );

    // ---- (b) strictly increasing wire serials across every serial-bearing event ----
    let serials: Vec<u32> = app.events.iter().filter_map(|(_, s)| *s).collect();
    assert_eq!(
        serials.len(),
        6,
        "six events carry a serial (kbd enter, ptr enter, 2 keys, 2 buttons)"
    );
    for w in serials.windows(2) {
        assert!(
            w[0] < w[1],
            "serials strictly increase: {} < {}",
            w[0],
            w[1]
        );
    }

    // ---- (c) the input-driven render landed at the very next present cycle ----
    let mf = marker_frame.unwrap();
    assert_eq!(
        mf.serial,
        baseline + 1,
        "marker rendered at the immediately following present cycle"
    );
    assert_eq!(
        mf.pixel(PX, PY).unwrap(),
        MARKER,
        "marker present at the injected pointer position"
    );
    assert_eq!(
        mf.pixel(1, 1).unwrap(),
        BASE,
        "background elsewhere stays BASE"
    );
    // No other present happened after the baseline — the burst caused exactly ONE prompt re-render.
    let after_baseline = h
        .captures
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f.serial > baseline)
        .count();
    assert_eq!(
        after_baseline, 1,
        "exactly one present after baseline (bounded, no redundant frames)"
    );

    save_frame("input_latency_ordering-marker", &mf);

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
            if !app.tl_drawn {
                app.tl_surface.attach(Some(&app.base_buffer), 0, 0);
                app.tl_surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.tl_surface.frame(qh, ());
                app.tl_surface.commit();
                app.tl_drawn = true;
            }
        }
    }
}
impl Dispatch<WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        e: <WlPointer as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                app.events.push((EvKind::PtrEnter, Some(serial)));
                let _ = (surface_x, surface_y);
                app.draw_marker(qh);
            }
            wl_pointer::Event::Motion { .. } => {
                app.events.push((EvKind::PtrMotion, None));
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state,
                ..
            } => {
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                app.events
                    .push((EvKind::Button(button, pressed), Some(serial)));
            }
            _ => {}
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
            wl_keyboard::Event::Enter { serial, .. } => {
                app.events.push((EvKind::KbdEnter, Some(serial)));
            }
            wl_keyboard::Event::Key {
                serial, key, state, ..
            } => {
                let pressed = matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                app.events.push((EvKind::Key(key, pressed), Some(serial)));
            }
            _ => {}
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
            app.tl_frame_done = true;
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
