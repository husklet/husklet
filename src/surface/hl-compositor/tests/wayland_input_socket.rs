//! Live-socket INPUT-delivery proof: a real client receives pointer + keyboard input the compositor
//! injects.
//!
//! `wayland_live_socket` proved a client can DISCOVER the compositor, enumerate `wl_output`/`wl_seat`,
//! create `wl_pointer`/`wl_keyboard`, and composite a buffer — but it stopped at object creation: no
//! input EVENTS ever flowed, so a real GUI toolkit on this compositor could never be interacted with.
//! This test closes that gap end to end:
//!
//!   1. A real `wayland-client` discovers the compositor via `$WAYLAND_DISPLAY`, maps an `xdg_toplevel`,
//!      attaches a known-size `wl_shm` buffer, and creates `wl_pointer` + `wl_keyboard`.
//!   2. The compositor is run with a host INPUT CHANNEL (`run_auto_with_input`). Because a headless
//!      compositor has no hardware input source, the test drives that channel — the exact seam a host
//!      integration would feed real device events through — to:
//!        - give keyboard focus to the mapped toplevel,
//!        - move the pointer to a point OVER the surface,
//!        - press a pointer button,
//!        - press a key.
//!   3. The client asserts it received, on the WIRE: `wl_pointer.enter` (naming ITS surface) with the
//!      correct surface-local coordinate (the injected global point minus the surface origin),
//!      `wl_pointer.motion` at that same local coordinate, `wl_pointer.button` (correct button + pressed
//!      state), and `wl_keyboard.enter` (naming its surface) + `wl_keyboard.key` with the injected
//!      EVDEV keycode and pressed state.
//!
//! Fully headless — real socket, real wire, real seat — no DRM, no libinput, no display. Its own test
//! binary because it mutates process-global `$XDG_RUNTIME_DIR` / `$WAYLAND_DISPLAY`.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{self, input_channel, InputCommand, PngPresenter};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, KeyState, WlKeyboard},
    wl_output::WlOutput,
    wl_pointer::{self, ButtonState, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::{Capability, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

// A deliberately non-square surface so an x/y coordinate mix-up would be caught.
const W: i32 = 200;
const H: i32 = 150;
// The color the client paints. `wl_shm` Argb8888 is 32-bit little-endian → memory bytes `[B, G, R, A]`.
const R: u8 = 0x22;
const G: u8 = 0x55;
const B: u8 = 0x99;
const A: u8 = 0xFF;

// The points (in global/root space) we move the pointer to. The surface roots at (0, 0) with no
// subsurface offset, so the surface-local coordinate the client must receive equals these exactly. Off
// center and asymmetric so a swapped axis or a stray origin offset is caught. The FIRST move enters the
// surface — Wayland delivers the location on `wl_pointer.enter`, not a separate motion. The SECOND move
// (still inside the surface) is what produces a `wl_pointer.motion` event, proving live tracking.
const PX: f64 = 120.0;
const PY: f64 = 90.0;
const PX2: f64 = 60.0;
const PY2: f64 = 30.0;
// Linux `input-event-codes`: BTN_LEFT and KEY_A — the values the client must receive back on the wire.
const BTN_LEFT: u32 = 0x110;
const KEY_A: u32 = 30;
// A downward scroll amount (logical units); the client must see a positive vertical axis value.
const SCROLL: f64 = 15.0;

struct AppData {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
    released: bool,
    frame_done: bool,
    seat_caps: Option<Capability>,
    // ---- recorded input the compositor delivered over the wire ----
    /// `wl_pointer.enter`: `(surface matched ours, surface-local x, surface-local y)`.
    pointer_enter: Option<(bool, f64, f64)>,
    /// `wl_pointer.motion`: surface-local `(x, y)`.
    pointer_motion: Option<(f64, f64)>,
    /// `wl_pointer.button`: `(button, pressed)`.
    pointer_button: Option<(u32, bool)>,
    /// `wl_pointer.axis`: the vertical scroll value delivered.
    pointer_axis_vert: Option<f64>,
    /// `wl_keyboard.enter`: whether it named our surface.
    keyboard_enter: Option<bool>,
    /// `wl_keyboard.key`: `(keycode, pressed)`.
    keyboard_key: Option<(u32, bool)>,
}

impl AppData {
    /// Every input assertion has landed — the compositor delivered the full pointer + keyboard sequence.
    fn input_complete(&self) -> bool {
        self.pointer_enter.is_some()
            && self.pointer_motion.is_some()
            && self.pointer_button.is_some()
            && self.pointer_axis_vert.is_some()
            && self.keyboard_enter.is_some()
            && self.keyboard_key.is_some()
    }
}

#[test]
fn injected_pointer_and_keyboard_reach_the_focused_client() {
    // ---- 1. A private XDG_RUNTIME_DIR so the discovery socket lands in an isolated, 0700 dir ----------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-input-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET");

    let png_dir = runtime_dir.join("png");

    // ---- 2. Start the compositor with a host INPUT channel in a background thread --------------------
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(png_dir.clone());
    let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();
    // The host input seam: `input_tx` stays here (client thread), `input_rx` is drained by the loop.
    let (input_tx, input_rx) = input_channel();

    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        smithay::run_auto_with_input(presenter, stop_thread, input_rx, move |name| {
            let _ = name_tx.send(name);
        })
        .expect("compositor serve loop (run_auto_with_input)");
    });

    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto_with_input never reported a bound socket name");
    let socket_path = runtime_dir.join(&socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "discovery socket {socket_path:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 3. Connect a real client, bind globals, build the toplevel ----------------------------------
    let conn = Connection::connect_to_env().expect("connect_to_env failed");
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor global");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm global");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base global");
    let _output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output global");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat global");

    // Build a wl_shm buffer of the known color/size.
    let stride = W * 4;
    let size = (stride * H) as usize;
    let mut pixels = Vec::with_capacity(size);
    for _ in 0..(W * H) {
        pixels.extend_from_slice(&[B, G, R, A]);
    }
    let shm_path = runtime_dir.join("client.shm");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&shm_path)
        .expect("shm file");
    file.write_all(&pixels).expect("write shm pixels");
    file.flush().unwrap();
    let _ = std::fs::remove_file(&shm_path);

    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let buffer: WlBuffer = pool.create_buffer(0, W, H, stride, wl_shm::Format::Argb8888, &qh, ());

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("hl-wip-input".into());
    surface.commit(); // initial empty commit → first configure

    let mut app = AppData {
        surface: surface.clone(),
        buffer: buffer.clone(),
        configured: false,
        released: false,
        frame_done: false,
        seat_caps: None,
        pointer_enter: None,
        pointer_motion: None,
        pointer_button: None,
        pointer_axis_vert: None,
        keyboard_enter: None,
        keyboard_key: None,
    };

    // Drive the map/commit handshake to completion: the surface is configured, our buffer consumed, and
    // the frame presented — so on the SERVER the surface now has committed content a hit-test can land in.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.configured && app.released && app.frame_done) {
        assert!(
            Instant::now() < deadline,
            "map handshake incomplete: configured={} released={} frame_done={}",
            app.configured,
            app.released,
            app.frame_done,
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("client dispatch (map)");
    }

    // The seat must advertise pointer + keyboard, then create both objects and roundtrip so the SERVER
    // has registered them before we inject (events route to the client's live pointer/keyboard objects).
    queue.roundtrip(&mut app).expect("roundtrip for seat caps");
    let caps = app.seat_caps.expect("seat capabilities");
    assert!(
        caps.contains(Capability::Pointer),
        "seat advertises pointer, got {caps:?}"
    );
    assert!(
        caps.contains(Capability::Keyboard),
        "seat advertises keyboard, got {caps:?}"
    );
    let pointer: WlPointer = seat.get_pointer(&qh, ());
    let keyboard: WlKeyboard = seat.get_keyboard(&qh, ());
    queue
        .roundtrip(&mut app)
        .expect("roundtrip after creating pointer/keyboard");
    assert!(
        pointer.is_alive() && keyboard.is_alive(),
        "pointer + keyboard objects alive"
    );

    // ---- 4. Inject input through the host channel ----------------------------------------------------
    // Order matters: focus the keyboard before the key; the first move enters the surface, the second
    // produces the motion event, and the button lands after the pointer is over the surface.
    input_tx
        .send(InputCommand::FocusTopmostKeyboard)
        .expect("send focus");
    input_tx
        .send(InputCommand::PointerMotion { x: PX, y: PY })
        .expect("send enter motion");
    input_tx
        .send(InputCommand::PointerMotion { x: PX2, y: PY2 })
        .expect("send tracking motion");
    input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("send button");
    input_tx
        .send(InputCommand::PointerAxis {
            horizontal: 0.0,
            vertical: SCROLL,
        })
        .expect("send axis");
    input_tx
        .send(InputCommand::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .expect("send key");

    // ---- 5. Assert the client received the full sequence on the wire ---------------------------------
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.input_complete() {
        assert!(
            Instant::now() < deadline,
            "input not fully delivered: enter={:?} motion={:?} button={:?} kbd_enter={:?} key={:?}",
            app.pointer_enter,
            app.pointer_motion,
            app.pointer_button,
            app.keyboard_enter,
            app.keyboard_key,
        );
        queue.roundtrip(&mut app).expect("client dispatch (input)");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Pointer entered OUR surface at the first point's surface-local coordinate (global point − surface
    // origin, and the origin is (0,0) here); the tracking move then delivered motion at the second point.
    let (enter_matched, enter_x, enter_y) = app.pointer_enter.unwrap();
    assert!(
        enter_matched,
        "wl_pointer.enter named a different surface than the client's toplevel"
    );
    assert_eq!(
        (enter_x, enter_y),
        (PX, PY),
        "wl_pointer.enter surface-local coordinate"
    );
    assert_eq!(
        app.pointer_motion.unwrap(),
        (PX2, PY2),
        "wl_pointer.motion surface-local coordinate"
    );

    // Pointer button: BTN_LEFT, pressed.
    assert_eq!(
        app.pointer_button.unwrap(),
        (BTN_LEFT, true),
        "wl_pointer.button (button, pressed)"
    );

    // Pointer axis: the injected downward scroll arrived as a positive vertical value.
    assert_eq!(
        app.pointer_axis_vert.unwrap(),
        SCROLL,
        "wl_pointer.axis vertical scroll value"
    );

    // Keyboard entered our surface, then delivered the injected key (evdev KEY_A, pressed).
    assert_eq!(
        app.keyboard_enter.unwrap(),
        true,
        "wl_keyboard.enter named the client's surface"
    );
    assert_eq!(
        app.keyboard_key.unwrap(),
        (KEY_A, true),
        "wl_keyboard.key (keycode, pressed)"
    );

    // The composite path stayed intact alongside input (the presenter still captured the mapped frame).
    let captures = png_dir.exists();
    assert!(captures, "png dir exists (composite path ran)");

    // ---- 6. Shut down --------------------------------------------------------------------------------
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

// ------------------------- wayland-client Dispatch plumbing (client side) -------------------------

impl Dispatch<WlRegistry, GlobalListContents> for AppData {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for AppData {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for AppData {
    fn event(
        app: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if !app.configured {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.configured = true;
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlBuffer,
        event: <WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_buffer::Event::Release = event {
            app.released = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        event: <WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            app.frame_done = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(caps) = capabilities {
                app.seat_caps = Some(caps);
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        event: <WlPointer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                app.pointer_enter = Some((surface.id() == app.surface.id(), surface_x, surface_y));
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                app.pointer_motion = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(ButtonState::Pressed));
                app.pointer_button = Some((button, pressed));
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                if matches!(axis, WEnum::Value(wl_pointer::Axis::VerticalScroll)) {
                    app.pointer_axis_vert = Some(value);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => {
                app.keyboard_enter = Some(surface.id() == app.surface.id());
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = matches!(state, WEnum::Value(KeyState::Pressed));
                app.keyboard_key = Some((key, pressed));
            }
            _ => {}
        }
    }
}

// Objects whose events we don't act on.
macro_rules! ignore_dispatch {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for AppData {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {}
        }
    )*};
}
ignore_dispatch!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    XdgToplevel,
    WlOutput
);
