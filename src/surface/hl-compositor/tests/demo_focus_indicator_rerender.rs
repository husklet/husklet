//! DEMO — `focus_indicator_rerender` (focus moves across THREE toplevels; each re-renders its indicator).
//!
//! Three independent Wayland clients (separate `Connection`s), each mapping a toplevel in a distinct base
//! color with a center "focus indicator" box. A client draws the box WHITE when it holds keyboard focus
//! (`wl_keyboard.enter`) and repaints it back to its base color when focus leaves (`wl_keyboard.leave`).
//! The test walks focus A → B → C and asserts, on PIXELS:
//!
//!   * the newly-focused client composites its indicator WHITE (it re-rendered a focus indicator);
//!   * the previously-focused client composites its indicator back to base (it re-rendered on defocus);
//!   * a key injected while B holds focus reaches ONLY B — A and C get no key.
//!
//! Proves focus changes propagate to the right client AND drive a real re-render — the visible "active
//! window" affordance every desktop shows, closed on composited pixels, not just the focus event log.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{CapturedFrame, InputCommand};
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
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 140;
const H: i32 = 110;
const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
const CX: i32 = W / 2;
const CY: i32 = H / 2;
const IND: i32 = 30;
const KEY_A: u32 = 30;

struct Client {
    surface: WlSurface,
    base: WlBuffer,
    focused: WlBuffer, // base fill + WHITE indicator box
    base_color: [u8; 4],
    drawn: bool,
    frame_done: bool,
    keys: Vec<(u32, bool)>,
}

impl Client {
    fn redraw(&mut self, focused: bool, qh: &QueueHandle<Client>) {
        self.surface
            .attach(Some(if focused { &self.focused } else { &self.base }), 0, 0);
        self.surface.damage(0, 0, W, H);
        let _cb: WlCallback = self.surface.frame(qh, ());
        self.surface.commit();
    }
}

fn spawn(
    dir: &std::path::Path,
    tag: &str,
    color: [u8; 4],
) -> (Connection, EventQueue<Client>, Client) {
    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<Client>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base = make_buffer(
        &shm,
        &qh,
        dir,
        &format!("{tag}-base"),
        W,
        H,
        &solid(W, H, color),
    );
    let mut foc = solid(W, H, color);
    fill_rect(&mut foc, W, H, CX - IND / 2, CY - IND / 2, IND, IND, WHITE);
    let focused = make_buffer(&shm, &qh, dir, &format!("{tag}-foc"), W, H, &foc);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("demo-focus-{tag}"));
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = Client {
        surface: surface.clone(),
        base,
        focused,
        base_color: color,
        drawn: false,
        frame_done: false,
        keys: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "client {tag} never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = queue.roundtrip(&mut app);
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(_kbd);
    std::mem::forget(seat);
    (conn, queue, app)
}

/// Latest captured frame (with `serial > after`) for the client whose base is `color`, indicator `center`.
/// The `after` floor matters because an unfocused frame (base==center color) is indistinguishable from
/// the client's INITIAL unfocused frame — a defocus repaint must be a NEW present, not the stale first one.
fn latest(
    caps: &std::sync::Arc<std::sync::Mutex<Vec<CapturedFrame>>>,
    base: [u8; 4],
    center: [u8; 4],
    after: u64,
) -> Option<CapturedFrame> {
    caps.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|f| {
            f.serial > after && f.width == W && f.pixel_is(1, 1, base) && f.pixel_is(CX, CY, center)
        })
        .cloned()
}

fn pump3(qs: [&mut EventQueue<Client>; 3], apps: [&mut Client; 3]) {
    let [qa, qb, qc] = qs;
    let [a, b, c] = apps;
    let _ = qa.roundtrip(a);
    let _ = qb.roundtrip(b);
    let _ = qc.roundtrip(c);
}

#[allow(clippy::too_many_arguments)]
fn await_focus_frame(
    caps: &std::sync::Arc<std::sync::Mutex<Vec<CapturedFrame>>>,
    qs: [&mut EventQueue<Client>; 3],
    apps: [&mut Client; 3],
    base: [u8; 4],
    center: [u8; 4],
    after: u64,
    what: &str,
) -> CapturedFrame {
    let [qa, qb, qc] = qs;
    let [a, b, c] = apps;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pump3([qa, qb, qc], [a, b, c]);
        if let Some(f) = latest(caps, base, center, after) {
            return f;
        }
        assert!(Instant::now() < deadline, "{what} never composited");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn focus_indicator_rerender() {
    let h = Harness::start("focus_indicator");

    const RED: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF];
    const GREEN: [u8; 4] = [0x20, 0xE0, 0x20, 0xFF];
    const BLUE: [u8; 4] = [0x30, 0x40, 0xE0, 0xFF];

    // Map A (index 0), B (index 1), C (index 2).
    let (_ca, mut qa, mut a) = spawn(&h.runtime_dir, "A", RED);
    let (_cb, mut qb, mut b) = spawn(&h.runtime_dir, "B", GREEN);
    let (_cc, mut qc, mut c) = spawn(&h.runtime_dir, "C", BLUE);

    // All three start UNFOCUSED (indicator == base color).
    await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        RED,
        RED,
        0,
        "A unfocused",
    );
    await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        GREEN,
        GREEN,
        0,
        "B unfocused",
    );
    await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        BLUE,
        BLUE,
        0,
        "C unfocused",
    );

    // ---- focus A → A's indicator goes WHITE ----
    h.input_tx
        .send(InputCommand::FocusToplevelIndex(0))
        .expect("focus A");
    let fa = await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        RED,
        WHITE,
        0,
        "A focused indicator",
    );
    assert_eq!(
        fa.pixel(CX, CY).unwrap(),
        WHITE,
        "A drew its focus indicator on gaining focus"
    );
    save_frame("focus_indicator-A-focused", &fa);

    // ---- focus B → A repaints indicator back to base, B goes WHITE ----
    h.input_tx
        .send(InputCommand::FocusToplevelIndex(1))
        .expect("focus B");
    // A's defocus repaint must be a NEW present after its focus frame (else the stale initial frame matches).
    let a_defocus = await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        RED,
        RED,
        fa.serial,
        "A defocused indicator",
    );
    let fb = await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        GREEN,
        WHITE,
        0,
        "B focused indicator",
    );
    assert_eq!(
        a_defocus.pixel(CX, CY).unwrap(),
        RED,
        "A cleared its indicator when focus left it"
    );
    assert_eq!(
        fb.pixel(CX, CY).unwrap(),
        WHITE,
        "B drew its focus indicator on gaining focus"
    );
    assert!(
        a_defocus.serial > fa.serial,
        "A's defocus repaint is a later present than its focus repaint"
    );

    // ---- key while B focused: only B ----
    h.input_tx
        .send(InputCommand::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .expect("key");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !b.keys.contains(&(KEY_A, true)) {
        assert!(Instant::now() < deadline, "B never received the key");
        pump3([&mut qa, &mut qb, &mut qc], [&mut a, &mut b, &mut c]);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(b.keys, vec![(KEY_A, true)], "focused B received the key");
    assert!(
        a.keys.is_empty(),
        "unfocused A received no key, got {:?}",
        a.keys
    );
    assert!(
        c.keys.is_empty(),
        "unfocused C received no key, got {:?}",
        c.keys
    );

    // ---- focus C → B clears, C goes WHITE ----
    h.input_tx
        .send(InputCommand::FocusToplevelIndex(2))
        .expect("focus C");
    let b_defocus = await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        GREEN,
        GREEN,
        fb.serial,
        "B defocused indicator",
    );
    let fc = await_focus_frame(
        &h.captures,
        [&mut qa, &mut qb, &mut qc],
        [&mut a, &mut b, &mut c],
        BLUE,
        WHITE,
        0,
        "C focused indicator",
    );
    assert_eq!(
        b_defocus.pixel(CX, CY).unwrap(),
        GREEN,
        "B cleared its indicator when focus left it"
    );
    assert_eq!(
        fc.pixel(CX, CY).unwrap(),
        WHITE,
        "C drew its focus indicator on gaining focus"
    );
    save_composited(
        "focus_indicator-final",
        3 * W + 40,
        H,
        [0x10, 0x10, 0x10, 0xFF],
        &[
            (&a_defocus, 0, 0),
            (&b_defocus, W + 20, 0),
            (&fc, 2 * W + 40, 0),
        ],
    );

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlRegistry, GlobalListContents> for Client {
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
impl Dispatch<XdgWmBase, ()> for Client {
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
impl Dispatch<XdgSurface, ()> for Client {
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
                app.redraw(false, qh);
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for Client {
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
impl Dispatch<WlKeyboard, ()> for Client {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        e: <WlKeyboard as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Enter { surface, .. } if surface.id() == app.surface.id() => {
                app.redraw(true, qh)
            }
            wl_keyboard::Event::Leave { surface, .. } if surface.id() == app.surface.id() => {
                app.redraw(false, qh)
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                app.keys
                    .push((key, matches!(state, WEnum::Value(KeyState::Pressed))));
            }
            _ => {}
        }
        let _ = app.base_color;
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Client {
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
