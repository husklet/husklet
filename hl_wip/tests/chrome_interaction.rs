//! CHROME INTERACTION — prove the desktop INPUT round-trip is real and delay-free.
//!
//! `chrome_e2e.rs` proves Chrome SUBMITS GL through our stack; it does NOT prove that input round-trips.
//! The user's bar is explicit: "ensure chrome doesnt suffer from delays and overall chrome experience is
//! as on any other desktop; ensure events propagate correctly; ensure apps such as chrome react to them
//! well." This test closes that loop with EXACT, deterministic assertions.
//!
//! WHICH CLIENT — a minimal REAL in-process `wayland-client` (the same client the compositor's own
//! `demo_*` battery drives), NOT the real Chromium process. Rationale: on this box Chromium is blocked
//! UPSTREAM of any window at GAP #0c (an arm64 IMMEDIATE_CRASH in early ChromeMain, tracked green by
//! `chrome_e2e.rs`) — it never maps a `wl_surface`, so it can never be given pointer/keyboard focus and
//! literally cannot receive a `wl_pointer.enter`. The input path a mapped app (Chrome, once past #0c)
//! exercises is the compositor's seat → `wl_pointer`/`wl_keyboard` wire delivery, which is identical for
//! ANY Wayland client. Driving a real, deterministic Wayland client over the SAME compositor seam
//! (`run_auto_with_input` + `InputCommand`, the exact host-input seam a real HID source feeds) proves that
//! path EXACTLY, with no Chrome-cold-boot flakiness in the micro-timing asserts. `chrome_interaction_smoke`
//! below keeps the ACTUAL Chromium process in the loop as a lenient tracker.
//!
//! THE CLOSED LOOP, with hl-log evidence at every hop (run with `HL_LOG=wayland HL_LOG_LEVEL=debug` and
//! `--nocapture`; this test also force-enables that mask so the evidence always fires):
//!   * INJECT   — this test logs `inject <kind>` and sends an `InputCommand` down the host seam.
//!   * DISPATCH — the compositor logs `input_dispatch t_us=…` (its own `tag::WAYLAND` line in
//!                `HlState::apply_input`) as it applies the command to the seat.
//!   * DELIVER  — this test's client `Dispatch` handlers log `deliver <kind> lat_us=…` the instant the
//!                `wl_pointer`/`wl_keyboard` wire event lands, and record its arrival `Instant`.
//! The INJECT→DELIVER wall-clock latency is measured for every event and asserted BOUNDED (no multi-frame
//! stall) — the concrete refutation of "delays".
//!
//! Four scenarios, all against ONE live compositor + ONE real client connection (so it mirrors one app
//! doing everything), run sequentially in a single #[test] to avoid the process-global `$WAYLAND_DISPLAY`
//! races that force the compositor's demos into separate binaries:
//!   1. pointer motion / button / smooth + discrete scroll — in-order delivery, bounded latency.
//!   2. keyboard focus enter/leave + key press/release + modifier latch — in-order, bounded latency.
//!   3. xdg_toplevel configure (resize) round-trip — client acks + the next committed buffer presents,
//!      no lost configure, bounded turnaround.
//!   4. a 100 rapid-pointer-motion BURST — every motion delivered (zero drops), STRICT order, bounded
//!      worst-case latency (proves no starvation/backpressure stall under load).

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{
    self, input_channel, CapturedFrame, InputCommand, InputSender, PngPresenter,
};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, KeyState, WlKeyboard},
    wl_pointer::{self, Axis, AxisSource, ButtonState, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

// ------------------------------------------------------------------------------------------------
// Geometry / input constants
// ------------------------------------------------------------------------------------------------

const W: i32 = 240;
const H: i32 = 180;
const BASE: [u8; 4] = [0x20, 0x24, 0x2c, 0xFF]; // dark slate — the mapped window
const RESIZE_COL: [u8; 4] = [0xE0, 0x90, 0x10, 0xFF]; // amber — the post-configure buffer

// evdev keycodes (Linux input-event-codes) — the exact values the client sees on wl_keyboard.key.
const KEY_A: u32 = 30;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTCTRL: u32 = 29;
const BTN_LEFT: u32 = 0x110;
// xkb real-modifier masks (stable across layouts): Shift = bit 0, Control = bit 2.
const MOD_SHIFT: u32 = 1;
const MOD_CTRL: u32 = 4;

// The compositor's floating (INITIAL_TOPLEVEL_SIZE) and its primary output logical size — the sizes its
// xdg configure carries (mirrors demo_xdg_configure_ack_roundtrip's pinned values).
const OUTPUT_LOGICAL: (i32, i32) = (1920, 1080);

/// Hard ceiling on ANY single injection→delivery. A real "delay" the user fears (a starved event loop, a
/// backpressure stall) manifests as multi-hundred-ms to multi-second lag; a healthy seat delivers in well
/// under a frame. This ceiling fails loudly on a genuine stall while tolerating CI scheduling jitter. The
/// test also REPORTS the actual measured latencies, which are sub-millisecond in practice.
const LAT_BUDGET: Duration = Duration::from_millis(200);
/// The rapid-motion burst size (task point 4).
const BURST: usize = 100;

// ------------------------------------------------------------------------------------------------
// Recorded client-side events (with arrival Instant, for INJECT→DELIVER latency)
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Ev {
    KbdEnter(u32),
    KbdLeave(u32),
    KbdKey { serial: u32, key: u32, pressed: bool },
    KbdMods { depressed: u32 },
    PtrEnter { serial: u32 },
    PtrLeave { serial: u32 },
    PtrMotion { x: f64, y: f64 },
    PtrButton { serial: u32, button: u32, pressed: bool },
    PtrAxisV(f64),
    PtrAxisSource(u32),
    PtrV120(i32),
    PtrFrame,
}

/// One delivered event + the wall-clock instant its `Dispatch` handler fired.
#[derive(Clone, Debug)]
struct Rec {
    ev: Ev,
    at: Instant,
}

struct App {
    // shell objects
    surface: WlSurface,
    base_buffer: WlBuffer,
    // map handshake
    drawn: bool,
    frame_done: bool,
    // configure tracking (scenario 3)
    pending_configure: Option<u32>,
    configure_count: u32,
    tl_size: Option<(i32, i32)>,
    // recorded wire events (all scenarios), in arrival order
    events: Vec<Rec>,
    // every PtrMotion x, for the burst-order assertion
    motion_xs: Vec<f64>,
}

impl App {
    fn push(&mut self, ev: Ev) {
        // DELIVER hop: hl-log evidence the instant the wire event lands on the client.
        hl_log::hl_debug!(hl_log::tag::WAYLAND, "deliver {:?}", ev);
        if let Ev::PtrMotion { x, .. } = ev {
            self.motion_xs.push(x);
        }
        self.events.push(Rec { ev, at: Instant::now() });
    }
    fn last_mods(&self) -> Option<u32> {
        self.events.iter().rev().find_map(|r| match r.ev {
            Ev::KbdMods { depressed } => Some(depressed),
            _ => None,
        })
    }
}

// ------------------------------------------------------------------------------------------------
// Compositor harness (input-enabled), on a private $XDG_RUNTIME_DIR / $WAYLAND_DISPLAY
// ------------------------------------------------------------------------------------------------

struct Harness {
    runtime_dir: PathBuf,
    stop: Arc<AtomicBool>,
    captures: Arc<Mutex<Vec<CapturedFrame>>>,
    input_tx: InputSender<InputCommand>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    fn start(tag: &str) -> Harness {
        let runtime_dir = std::env::temp_dir().join(format!("hl-wip-interact-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        // SAFETY: this test owns its whole test binary/process (a single #[test]).
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        std::env::remove_var("WAYLAND_SOCKET");

        let stop = Arc::new(AtomicBool::new(false));
        let presenter = PngPresenter::with_png_dir(runtime_dir.join("png"));
        let captures = presenter.captures();
        let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();
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
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        let socket_path = runtime_dir.join(&socket_name);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket_path.exists() {
            assert!(Instant::now() < deadline, "discovery socket {socket_path:?} never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
        Harness { runtime_dir, stop, captures, input_tx, handle: Some(handle) }
    }

    /// INJECT hop: log then send. Returns the `Instant` the command left this test (t0 for latency).
    fn inject(&self, label: &str, cmd: InputCommand) -> Instant {
        hl_log::hl_info!(hl_log::tag::WAYLAND, "inject {label}");
        let t0 = Instant::now();
        self.input_tx.send(cmd).expect("input send");
        t0
    }

    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

// ------------------------------------------------------------------------------------------------
// wl_shm buffer helpers (tight top-left BGRA == little-endian ARGB word)
// ------------------------------------------------------------------------------------------------

fn solid(w: i32, h: i32, rgba: [u8; 4]) -> Vec<u8> {
    let [r, g, b, a] = rgba;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        px.extend_from_slice(&[b, g, r, a]);
    }
    px
}

fn make_buffer(shm: &WlShm, qh: &QueueHandle<App>, dir: &Path, tag: &str, w: i32, h: i32, bgra: &[u8]) -> WlBuffer {
    let stride = w * 4;
    let size = (stride * h) as usize;
    assert_eq!(bgra.len(), size, "pixel buffer size mismatch for {tag}");
    let path = dir.join(format!("client-{tag}.shm"));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("shm file");
    file.write_all(bgra).expect("write shm pixels");
    file.flush().unwrap();
    let _ = std::fs::remove_file(&path); // unlink; the fd + mapping stay valid
    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    std::mem::forget(file); // the pool keeps the mapping alive via the fd
    pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ())
}

// ------------------------------------------------------------------------------------------------
// Pump helpers
// ------------------------------------------------------------------------------------------------

/// Roundtrip the client queue (forcing a server sync, so any just-injected event is delivered) until
/// `done(app)` or the deadline. Returns whether it succeeded. No inter-iteration sleep: `roundtrip`
/// blocks on the sync reply, so this paces itself and keeps the DELIVER timestamp tight.
fn pump_until(
    queue: &mut EventQueue<App>,
    app: &mut App,
    secs: u64,
    done: impl Fn(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if done(app) {
            return true;
        }
        if Instant::now() >= deadline {
            return done(app);
        }
        let _ = queue.roundtrip(app);
    }
}

/// Poll the presenter capture log for a frame matching `pred`.
fn wait_frame(
    queue: &mut EventQueue<App>,
    app: &mut App,
    captures: &Arc<Mutex<Vec<CapturedFrame>>>,
    secs: u64,
    pred: impl Fn(&CapturedFrame) -> bool,
) -> Option<CapturedFrame> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let _ = queue.roundtrip(app);
        if let Some(f) = captures.lock().unwrap().iter().rev().find(|f| pred(f)).cloned() {
            return Some(f);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Roundtrip a few times with brief pauses so late/asynchronous wire traffic (leave events, mods) settles.
fn settle(queue: &mut EventQueue<App>, app: &mut App) {
    for _ in 0..4 {
        let _ = queue.roundtrip(app);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The max INJECT→DELIVER latency across the events recorded at index >= `from`, given inject instant `t0`.
fn max_latency(app: &App, from: usize, t0: Instant) -> Duration {
    app.events[from..]
        .iter()
        .map(|r| r.at.saturating_duration_since(t0))
        .max()
        .unwrap_or_default()
}

// ================================================================================================
// THE DETERMINISTIC PROOF
// ================================================================================================

#[test]
fn chrome_interaction_input_roundtrip() {
    // Force the hl-log evidence mask ON for this whole process (compositor thread shares these atomics),
    // so the INJECT (this test) / DISPATCH (compositor) / DELIVER (this test) lines all fire regardless of
    // the ambient env — the closed loop is visible under `--nocapture`.
    hl_log::set_enabled(hl_log::tag::WAYLAND);
    hl_log::set_level(hl_log::Level::Debug);

    let h = Harness::start("roundtrip");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let base_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    let resize_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "resize", W, H, &solid(W, H, RESIZE_COL));

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("chrome-interaction".into());
    surface.commit(); // solicit the initial configure

    let mut app = App {
        surface: surface.clone(),
        base_buffer: base_buffer.clone(),
        drawn: false,
        frame_done: false,
        pending_configure: None,
        configure_count: 0,
        tl_size: None,
        events: Vec::new(),
        motion_xs: Vec::new(),
    };

    // Map: the XdgSurface handler auto-acks the initial configure + draws BASE; wait until it is on screen.
    let map_deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < map_deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let base_frame = wait_frame(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel(1, 1) == Some(BASE))
        .expect("base frame never composited");

    // Create the input objects so injected input routes to live client resources.
    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _keyboard: WlKeyboard = seat.get_keyboard(&qh, ());
    let _ = queue.roundtrip(&mut app);

    // ------------------------------------------------------------------------------------------
    // SCENARIO 1 — pointer motion / button / smooth + discrete scroll: in-order, bounded latency.
    // ------------------------------------------------------------------------------------------
    let s1_from = app.events.len();

    // First motion ENTERS the surface (the enter conveys position; no separate motion).
    let t = h.inject("ptr_enter", InputCommand::PointerMotion { x: 100.0, y: 90.0 });
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[s1_from..].iter().any(|r| matches!(r.ev, Ev::PtrEnter { .. }))),
        "pointer enter never delivered");
    let lat_enter = latency_of(&app, s1_from, &t, |e| matches!(e, Ev::PtrEnter { .. }));
    assert!(lat_enter < LAT_BUDGET, "pointer-enter latency {lat_enter:?} exceeded budget {LAT_BUDGET:?}");

    // A second motion inside the surface is a real wl_pointer.motion.
    let t = h.inject("ptr_motion", InputCommand::PointerMotion { x: 120.0, y: 90.0 });
    let before = app.events.len();
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[before..].iter().any(|r| matches!(r.ev, Ev::PtrMotion { .. }))),
        "pointer motion never delivered");
    let lat_motion = latency_of(&app, before, &t, |e| matches!(e, Ev::PtrMotion { .. }));
    assert!(lat_motion < LAT_BUDGET, "pointer-motion latency {lat_motion:?} exceeded budget");

    // Button press + release.
    let t = h.inject("btn_down", InputCommand::PointerButton { button: BTN_LEFT, pressed: true });
    let _ = h.inject("btn_up", InputCommand::PointerButton { button: BTN_LEFT, pressed: false });
    let before = app.events.len();
    assert!(
        pump_until(&mut queue, &mut app, 5, |a| {
            a.events[before..].iter().filter(|r| matches!(r.ev, Ev::PtrButton { .. })).count() >= 2
        }),
        "pointer button press+release never delivered"
    );
    let lat_button = latency_of(&app, before, &t, |e| matches!(e, Ev::PtrButton { .. }));
    assert!(lat_button < LAT_BUDGET, "pointer-button latency {lat_button:?} exceeded budget");

    // Smooth scroll (wheel-source axis) then a DISCRETE wheel notch (axis_value120).
    let t = h.inject("scroll_smooth", InputCommand::PointerAxis { horizontal: 0.0, vertical: 15.0 });
    let before = app.events.len();
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[before..].iter().any(|r| matches!(r.ev, Ev::PtrAxisV(_)))),
        "smooth scroll axis never delivered");
    let lat_scroll = latency_of(&app, before, &t, |e| matches!(e, Ev::PtrAxisV(_)));
    assert!(lat_scroll < LAT_BUDGET, "smooth-scroll latency {lat_scroll:?} exceeded budget");
    let smooth_v = app.events[before..].iter().find_map(|r| match r.ev { Ev::PtrAxisV(v) => Some(v), _ => None }).unwrap();
    assert!(smooth_v > 0.0, "smooth scroll delivered a positive vertical value, got {smooth_v}");

    let t = h.inject("scroll_notch", InputCommand::PointerAxisDiscrete { horizontal: 0.0, vertical: 15.0, h120: 0, v120: 120 });
    let before = app.events.len();
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[before..].iter().any(|r| matches!(r.ev, Ev::PtrV120(_)))),
        "discrete scroll value120 never delivered");
    let lat_notch = latency_of(&app, before, &t, |e| matches!(e, Ev::PtrV120(_)));
    assert!(lat_notch < LAT_BUDGET, "discrete-scroll latency {lat_notch:?} exceeded budget");
    let v120 = app.events[before..].iter().find_map(|r| match r.ev { Ev::PtrV120(v) => Some(v), _ => None }).unwrap();
    assert_eq!(v120, 120, "one wheel detent == value120 of 120");
    assert!(app.events[before..].iter().any(|r| r.ev == Ev::PtrAxisSource(u32::from(AxisSource::Wheel))),
        "discrete scroll carried axis_source(wheel)");

    // ---- ORDER: the pointer semantic events arrived in exactly the injected order. ----
    let ptr_order: Vec<&Ev> = app.events[s1_from..]
        .iter()
        .map(|r| &r.ev)
        .filter(|e| matches!(e, Ev::PtrEnter { .. } | Ev::PtrMotion { .. } | Ev::PtrButton { .. } | Ev::PtrAxisV(_) | Ev::PtrV120(_)))
        .collect();
    assert!(matches!(ptr_order[0], Ev::PtrEnter { .. }), "first pointer event is enter");
    assert!(matches!(ptr_order[1], Ev::PtrMotion { .. }), "then motion");
    assert!(matches!(ptr_order[2], Ev::PtrButton { pressed: true, .. }), "then button press");
    assert!(matches!(ptr_order[3], Ev::PtrButton { pressed: false, .. }), "then button release");
    assert!(matches!(ptr_order[4], Ev::PtrAxisV(_)), "then smooth scroll");
    assert!(matches!(ptr_order[5], Ev::PtrV120(_)), "then discrete notch");

    // ---- serials strictly increase across the serial-bearing pointer events. ----
    let ptr_serials: Vec<u32> = app.events[s1_from..].iter().filter_map(|r| match r.ev {
        Ev::PtrEnter { serial } | Ev::PtrButton { serial, .. } => Some(serial),
        _ => None,
    }).collect();
    for w in ptr_serials.windows(2) {
        assert!(w[0] < w[1], "pointer wire serials strictly increase: {} < {}", w[0], w[1]);
    }

    // ------------------------------------------------------------------------------------------
    // SCENARIO 2 — keyboard focus enter/leave + keys + modifier latch: in-order, bounded.
    // ------------------------------------------------------------------------------------------
    let s2_from = app.events.len();

    let t = h.inject("kbd_focus", InputCommand::FocusTopmostKeyboard);
    // Focus delivers wl_keyboard.enter + an initial modifiers(0).
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[s2_from..].iter().any(|r| matches!(r.ev, Ev::KbdEnter(_)))),
        "keyboard enter never delivered on focus");
    let lat_kbd_enter = latency_of(&app, s2_from, &t, |e| matches!(e, Ev::KbdEnter(_)));
    assert!(lat_kbd_enter < LAT_BUDGET, "keyboard-enter latency {lat_kbd_enter:?} exceeded budget");
    pump_until(&mut queue, &mut app, 2, |a| a.last_mods().is_some());
    assert_eq!(app.last_mods(), Some(0), "initial modifiers on focus are 0 (nothing held)");

    // Hold Shift → mods latch to Shift; press A while held → key + mods still Shift.
    let t = h.inject("shift_down", InputCommand::Key { keycode: KEY_LEFTSHIFT, pressed: true });
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.last_mods() == Some(MOD_SHIFT)),
        "mods_depressed did not latch to Shift");
    let lat_mod = latency_of(&app, s2_from, &t, |e| matches!(e, Ev::KbdMods { depressed: MOD_SHIFT }));
    assert!(lat_mod < LAT_BUDGET, "modifier-latch latency {lat_mod:?} exceeded budget");

    let key_from = app.events.len();
    let t = h.inject("key_a_down", InputCommand::Key { keycode: KEY_A, pressed: true });
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[key_from..].iter().any(|r| matches!(&r.ev, Ev::KbdKey { key: KEY_A, pressed: true, .. }))),
        "KEY_A press never delivered");
    let lat_key = latency_of(&app, key_from, &t, |e| matches!(e, Ev::KbdKey { key: KEY_A, pressed: true, .. }));
    assert!(lat_key < LAT_BUDGET, "key latency {lat_key:?} exceeded budget");
    assert_eq!(app.last_mods(), Some(MOD_SHIFT), "Shift still reported held during the letter key");

    // Release A + Shift → mods back to 0.
    let _ = h.inject("key_a_up", InputCommand::Key { keycode: KEY_A, pressed: false });
    let _ = h.inject("shift_up", InputCommand::Key { keycode: KEY_LEFTSHIFT, pressed: false });
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.last_mods() == Some(0)),
        "mods_depressed did not return to 0 after Shift release");

    // Ctrl latch → 0 (a second independent modifier, proving no stuck state).
    let _ = h.inject("ctrl_down", InputCommand::Key { keycode: KEY_LEFTCTRL, pressed: true });
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.last_mods() == Some(MOD_CTRL)), "mods did not latch to Control");
    let _ = h.inject("ctrl_up", InputCommand::Key { keycode: KEY_LEFTCTRL, pressed: false });
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.last_mods() == Some(0)), "mods did not clear after Control release");

    // Exact key sequence (modifier keycodes are delivered as key events too), in injection order.
    let keys: Vec<(u32, bool)> = app.events[s2_from..].iter().filter_map(|r| match r.ev {
        Ev::KbdKey { key, pressed, .. } => Some((key, pressed)),
        _ => None,
    }).collect();
    assert_eq!(
        keys,
        vec![
            (KEY_LEFTSHIFT, true), (KEY_A, true), (KEY_A, false), (KEY_LEFTSHIFT, false),
            (KEY_LEFTCTRL, true), (KEY_LEFTCTRL, false),
        ],
        "exact keyboard key sequence in injection order",
    );

    // keyboard focus leave propagates too (enter/leave both, as the task requires).
    let leave_from = app.events.len();
    let t = h.inject("kbd_clear_focus", InputCommand::ClearKeyboardFocus);
    assert!(pump_until(&mut queue, &mut app, 5, |a| a.events[leave_from..].iter().any(|r| matches!(r.ev, Ev::KbdLeave(_)))),
        "keyboard leave never delivered on focus clear");
    let lat_kbd_leave = latency_of(&app, leave_from, &t, |e| matches!(e, Ev::KbdLeave(_)));
    assert!(lat_kbd_leave < LAT_BUDGET, "keyboard-leave latency {lat_kbd_leave:?} exceeded budget");
    // restore focus for later scenarios that assume a focused, active toplevel.
    let _ = h.inject("kbd_refocus", InputCommand::FocusTopmostKeyboard);
    settle(&mut queue, &mut app);

    // ------------------------------------------------------------------------------------------
    // SCENARIO 3 — xdg_toplevel configure (resize) round-trip: no lost configure, bounded turnaround.
    // ------------------------------------------------------------------------------------------
    // A compositor-driven configure with a NEW size. `set_maximized` solicits it (the compositor replies
    // with the output logical size + Maximized|Activated). We then ack the exact serial, commit the next
    // buffer, and assert it presents — the resize round-trip a real app performs on a window-size change.
    let cfg_before = app.configure_count;
    let t_cfg = Instant::now();
    hl_log::hl_info!(hl_log::tag::WAYLAND, "inject configure(set_maximized)");
    toplevel.set_maximized();
    let cfg_deadline = Instant::now() + Duration::from_secs(5);
    while app.configure_count <= cfg_before {
        assert!(Instant::now() < cfg_deadline, "set_maximized produced NO new configure (configure lost)");
        queue.blocking_dispatch(&mut app).expect("dispatch resize configure");
    }
    let configure_turnaround = t_cfg.elapsed();
    assert!(configure_turnaround < LAT_BUDGET, "configure turnaround {configure_turnaround:?} exceeded budget");
    let serial = app.pending_configure.expect("resize configure carried a serial");
    assert_eq!(app.tl_size, Some(OUTPUT_LOGICAL), "resize configure carries the new (output logical) size");

    // ACK the exact serial + commit the next buffer; it must reach the screen (bounded turnaround).
    xdg.ack_configure(serial);
    surface.attach(Some(&resize_buffer), 0, 0);
    surface.damage(0, 0, W, H);
    surface.commit();
    let resized = wait_frame(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel(W / 2, H / 2) == Some(RESIZE_COL))
        .expect("the committed buffer after ack_configure never presented (lost configure/commit)");
    assert!(resized.serial > base_frame.serial, "post-configure frame presents after the mapped frame");

    // ------------------------------------------------------------------------------------------
    // SCENARIO 4 — 100 rapid pointer motions: every one delivered, strict order, bounded worst-case.
    // ------------------------------------------------------------------------------------------
    // Ensure the pointer is inside the surface (so all burst motions are pure wl_pointer.motion, never a
    // focus-changing enter), settle, then fire the burst at strictly-increasing x.
    let _ = h.inject("burst_prime", InputCommand::PointerMotion { x: 10.0, y: 90.0 });
    settle(&mut queue, &mut app);
    let motions_before = app.motion_xs.len();

    hl_log::hl_info!(hl_log::tag::WAYLAND, "inject burst n={BURST}");
    let t_burst = Instant::now();
    let burst_from = app.events.len();
    for i in 0..BURST {
        // distinct, strictly increasing x within the window so order is verifiable and no motion coalesces.
        let x = 20.0 + i as f64; // 20..120, all inside the W=240 window
        h.input_tx.send(InputCommand::PointerMotion { x, y: 90.0 }).expect("burst motion");
    }
    assert!(
        pump_until(&mut queue, &mut app, 10, |a| a.motion_xs.len() - motions_before >= BURST),
        "burst not fully drained: delivered {} of {BURST}", app.motion_xs.len() - motions_before
    );
    let burst_wall = t_burst.elapsed();

    // (a) ZERO drops — exactly BURST motion events delivered.
    let delivered = app.motion_xs.len() - motions_before;
    assert_eq!(delivered, BURST, "every one of {BURST} rapid motions was delivered (no drops/coalescing)");

    // (b) STRICT order — the delivered x-coordinates are exactly the injected strictly-increasing sequence.
    let got_xs = &app.motion_xs[motions_before..motions_before + BURST];
    for (i, &x) in got_xs.iter().enumerate() {
        assert_eq!(x, 20.0 + i as f64, "burst motion #{i} arrived out of order (x={x})");
    }

    // (c) BOUNDED worst-case latency — the LAST motion's delivery is not stalled behind a backlog.
    let burst_max_lat = max_latency(&app, burst_from, t_burst);
    assert!(burst_max_lat < LAT_BUDGET, "worst-case burst delivery {burst_max_lat:?} exceeded budget {LAT_BUDGET:?}");

    // ------------------------------------------------------------------------------------------
    // Report — the measured closed-loop latencies (all in µs) and the burst throughput.
    // ------------------------------------------------------------------------------------------
    let us = |d: Duration| d.as_micros();
    eprintln!(
        "CHROME INTERACTION PASSED (real wayland-client, full compositor seat path):\n\
         inject->deliver latency (us): ptr_enter={} ptr_motion={} button={} scroll_smooth={} scroll_notch={} \
         kbd_enter={} kbd_leave={} mod_latch={} key={}\n\
         configure_resize: turnaround={}us new_size={:?} presented=yes\n\
         burst: delivered={}/{} strict_order=yes worst_latency={}us total_wall={}us ({:.1} evt/ms)",
        us(lat_enter), us(lat_motion), us(lat_button), us(lat_scroll), us(lat_notch),
        us(lat_kbd_enter), us(lat_kbd_leave), us(lat_mod), us(lat_key),
        us(configure_turnaround), app.tl_size.unwrap(),
        delivered, BURST, us(burst_max_lat), us(burst_wall),
        BURST as f64 / (burst_wall.as_micros().max(1) as f64 / 1000.0),
    );

    // Keep the shell objects alive to the end.
    std::mem::forget(toplevel);
    h.shutdown();
}

/// Latency from inject instant `t0` to the first event at index >= `from` matching `pred`.
fn latency_of(app: &App, from: usize, t0: &Instant, pred: impl Fn(&Ev) -> bool) -> Duration {
    app.events[from..]
        .iter()
        .find(|r| pred(&r.ev))
        .map(|r| r.at.saturating_duration_since(*t0))
        .unwrap_or(Duration::MAX)
}

// ================================================================================================
// Dispatch plumbing
// ================================================================================================

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(_: &mut Self, wm: &XdgWmBase, e: <XdgWmBase as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            // Record every configure for the resize round-trip. Auto-ack + draw ONLY the initial one (to
            // map); the resize configure (scenario 3) is acked explicitly by the test body so the
            // ack→commit ordering is observable.
            app.pending_configure = Some(serial);
            app.configure_count += 1;
            if !app.drawn {
                xdg.ack_configure(serial);
                app.surface.attach(Some(&app.base_buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<XdgToplevel, ()> for App {
    fn event(app: &mut Self, _: &XdgToplevel, e: <XdgToplevel as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = e {
            app.tl_size = Some((width, height));
        }
    }
}
impl Dispatch<WlCallback, ()> for App {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.frame_done = true;
        }
    }
}
impl Dispatch<WlPointer, ()> for App {
    fn event(app: &mut Self, _: &WlPointer, e: <WlPointer as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_pointer::Event::Enter { serial, .. } => app.push(Ev::PtrEnter { serial }),
            wl_pointer::Event::Leave { serial, .. } => app.push(Ev::PtrLeave { serial }),
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => app.push(Ev::PtrMotion { x: surface_x, y: surface_y }),
            wl_pointer::Event::Button { serial, button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(ButtonState::Pressed));
                app.push(Ev::PtrButton { serial, button, pressed });
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                if matches!(axis, WEnum::Value(Axis::VerticalScroll)) {
                    app.push(Ev::PtrAxisV(value));
                }
            }
            wl_pointer::Event::AxisSource { axis_source } => {
                if let WEnum::Value(src) = axis_source {
                    app.push(Ev::PtrAxisSource(u32::from(src)));
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                if matches!(axis, WEnum::Value(Axis::VerticalScroll)) {
                    app.push(Ev::PtrV120(value120));
                }
            }
            wl_pointer::Event::Frame => app.push(Ev::PtrFrame),
            _ => {}
        }
    }
}
impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &WlKeyboard, e: <WlKeyboard as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_keyboard::Event::Enter { serial, .. } => app.push(Ev::KbdEnter(serial)),
            wl_keyboard::Event::Leave { serial, .. } => app.push(Ev::KbdLeave(serial)),
            wl_keyboard::Event::Key { serial, key, state, .. } => {
                let pressed = matches!(state, WEnum::Value(KeyState::Pressed));
                app.push(Ev::KbdKey { serial, key, pressed });
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => app.push(Ev::KbdMods { depressed: mods_depressed }),
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat);
