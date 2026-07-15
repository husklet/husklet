//! DEMO (batch-3) — `present_throughput` (every committed frame presents exactly once, in order).
//!
//! A single client maps a toplevel, then commits `M` DISTINCT frames back-to-back (each a solid color
//! whose red channel encodes its index, so every frame is uniquely identifiable). For each committed
//! frame the test waits for the composited capture whose pixels match THAT frame's buffer before
//! committing the next, so at most one frame is ever in flight. It then asserts, across the whole run:
//!
//!   * EXACTLY `M` presents happened after the initial map (`captures.len() == M + 1`) — no frame was
//!     dropped (a committed buffer that never presented) and none was duplicated (a present with no
//!     matching commit, or the same frame presented twice);
//!   * the present serials are the dense strictly-increasing sequence `1..=M+1` — no gap (dropped) and
//!     no repeat (duplicated);
//!   * each captured frame's FULL pixel buffer is byte-identical to the buffer the client committed for
//!     it, in commit order — the content the client asked to show is exactly what reached the screen.
//!
//! This locks the commit→present throughput path: the vsync throttle may DEFER a frame (a burst within
//! one refresh interval coalesces), but here — one frame in flight at a time — every distinct frame must
//! still reach the presenter exactly once and in order.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 80;
const H: i32 = 60;
const M: usize = 24; // distinct frames committed after the map (>= 20 per the brief)
const BASE: [u8; 4] = [0xC8, 0xC8, 0xC8, 0xFF]; // light gray — the map frame (red 0xC8, never a frame color)

/// The unique solid color for committed frame `i` (0-based): red channel encodes the index so the
/// captured frame is unambiguously identifiable (red 0x0A..0x21 for M=24, all distinct from BASE's 0xC8).
fn frame_color(i: usize) -> [u8; 4] {
    [0x0A + i as u8, 0x40, 0xC0, 0xFF]
}

/// A tight top-left RGBA canvas filled with a solid color — the exact bytes a captured frame must carry
/// (the presenter stores deposited pixels as RGBA; `common::solid` produces the BGRA the shm buffer holds).
fn solid_rgba(rgba: [u8; 4]) -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..(W * H) {
        px.extend_from_slice(&rgba);
    }
    px
}

struct App {
    surface: WlSurface,
    base_buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
}

#[test]
fn present_throughput() {
    let h = Harness::start("present_throughput");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let base_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "base", W, H, &solid(W, H, BASE));
    // One pre-drawn buffer per distinct frame.
    let frame_buffers: Vec<WlBuffer> = (0..M)
        .map(|i| make_buffer(&shm, &qh, &h.runtime_dir, &format!("f{i}"), W, H, &solid(W, H, frame_color(i))))
        .collect();

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-throughput".into());
    surface.commit();

    let mut app = App { surface: surface.clone(), base_buffer: base_buffer.clone(), drawn: false, frame_done: false };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // The initial map frame — BASE. This is present #1.
    let base_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, BASE))
        .expect("map (BASE) frame never composited");
    assert_eq!(base_frame.serial, 1, "map frame is the first present");

    // Commit M distinct frames, ONE in flight at a time: wait for frame i to present before committing i+1.
    for (i, buf) in frame_buffers.iter().enumerate() {
        let color = frame_color(i);
        app.surface.attach(Some(buf), 0, 0);
        app.surface.damage(0, 0, W, H);
        let _cb: WlCallback = app.surface.frame(&qh, ());
        app.surface.commit();

        let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == W && f.pixel_is(1, 1, color))
            .unwrap_or_else(|| panic!("committed frame {i} (color {color:?}) never presented"));
        // Exact content: the whole captured buffer equals what the client committed for this frame.
        assert_eq!(frame.rgba, solid_rgba(color), "frame {i} pixels match its committed buffer exactly");
    }

    // ---- whole-run assertions: exactly M+1 presents, dense monotonic serials, exact ordered content ----
    let caps = h.captures.lock().unwrap().clone();
    assert_eq!(caps.len(), M + 1, "exactly M+1 presents (1 map + {M} frames): no dropped/duplicated frame");

    // Serials are the dense strictly-increasing run 1..=M+1 — a gap would be a dropped present, a repeat a
    // duplicate.
    for (idx, f) in caps.iter().enumerate() {
        assert_eq!(f.serial, idx as u64 + 1, "present #{idx} has serial {}", idx as u64 + 1);
    }

    // Content in present order: map BASE, then frame 0..M in commit order, each an exact buffer match.
    assert_eq!(caps[0].rgba, solid_rgba(BASE), "present #0 is the BASE map frame");
    for i in 0..M {
        assert_eq!(caps[i + 1].rgba, solid_rgba(frame_color(i)), "present #{} is committed frame {i}", i + 1);
    }

    // Visual confirmation: first, middle, last committed frames.
    save_frame("present_throughput-first", &caps[1]);
    save_frame("present_throughput-middle", &caps[1 + M / 2]);
    save_frame("present_throughput-last", &caps[M]);

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
                app.surface.attach(Some(&app.base_buffer), 0, 0);
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
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel);
