//! DEMO — `keyboard_repeat_info` (the seat advertises its key-repeat cadence, exactly).
//!
//! When a client creates a `wl_keyboard` (v4+), the compositor must tell it the key-repeat cadence via
//! `wl_keyboard.repeat_info(rate, delay)` — the rate (keys/second) and the initial delay (ms) the client
//! uses to synthesize auto-repeat locally. A client that never hears it cannot repeat keys. The adapter
//! builds its seat keyboard with `add_keyboard(cfg, delay=200ms, rate=25/s)`, so the test asserts the
//! client receives EXACTLY `repeat_info(rate=25, delay=200)`, plus the `keymap` handshake (an xkb_v1
//! keymap fd) every keyboard needs. This locks the auto-repeat contract real toolkits depend on.

mod common;
use common::*;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, KeymapFormat, WlKeyboard},
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

const W: i32 = 120;
const H: i32 = 90;
const COLOR: [u8; 4] = [0x40, 0xC0, 0x80, 0xFF];
// The adapter's `add_keyboard(cfg, repeat_delay=200, repeat_rate=25)` → repeat_info(rate=25, delay=200).
const EXPECT_RATE: i32 = 25;
const EXPECT_DELAY: i32 = 200;

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    repeat_info: Option<(i32, i32)>,
    keymap_ok: bool,
}

#[test]
fn keyboard_repeat_info() {
    let h = Harness::start("keyboard_repeat_info");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // Bind the seat at v4+ so repeat_info is in-protocol.
    let seat: WlSeat = globals.bind(&qh, 4..=9, ()).expect("wl_seat v4+");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "kbd", W, H, &solid(W, H, COLOR));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-repeat-info".into());
    // Creating the keyboard is what solicits keymap + repeat_info.
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        repeat_info: None,
        keymap_ok: false,
    };

    // Pump until repeat_info + keymap arrive (both sent at keyboard creation). The map handshake runs
    // alongside (the configure handler attaches the buffer), keeping the client well-behaved.
    let ok = pump_while(&mut queue, &mut app, 5, |a| {
        a.repeat_info.is_some() && a.keymap_ok
    });
    assert!(ok, "wl_keyboard.repeat_info / keymap never arrived");
    assert_eq!(
        app.repeat_info,
        Some((EXPECT_RATE, EXPECT_DELAY)),
        "repeat_info carries the seat's exact (rate, delay)"
    );
    assert!(app.keymap_ok, "the keyboard received an xkb_v1 keymap fd");

    // Confirm the client is actually live (it mapped a frame) — repeat_info is not from a half-open client.
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W
            && f.pixel_is(1, 1, COLOR))
        .is_some(),
        "client never mapped a frame",
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
        _: &mut Self,
        _: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
            wl_keyboard::Event::RepeatInfo { rate, delay } => app.repeat_info = Some((rate, delay)),
            wl_keyboard::Event::Keymap { format, size, .. } => {
                app.keymap_ok = matches!(format, WEnum::Value(KeymapFormat::XkbV1)) && size > 0;
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
