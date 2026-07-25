//! DEMO 5 — `animation_fluency`.
//!
//! A client animates a colored rect sliding across the surface over several commits, each paced by a
//! `wl_surface.frame` callback (the client draws frame N+1 only after the compositor releases frame N's
//! callback). The test asserts that consecutive CAPTURED frames show the rect at successively different,
//! CORRECT positions (moving monotonically, not stuck, not garbage), and that the frame-callback pacing
//! released every frame (the animation self-drove to completion). This proves the compose/pace loop
//! delivers a fluent animation to a real client end to end.

mod client_harness;
use client_harness::*;

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

const W: i32 = 240;
const H: i32 = 80;
const BASE: [u8; 4] = [0x18, 0x18, 0x28, 0xFF]; // dark navy
const RECT: [u8; 4] = [0xF0, 0x80, 0x10, 0xFF]; // orange
const RW: i32 = 24;
const RH: i32 = 40;
const RY: i32 = (H - RH) / 2;
const N: usize = 6;
const X0: i32 = 10;
const STEP: i32 = 36;

/// The animated rect's left edge on frame `i`.
fn rect_x(i: usize) -> i32 {
    X0 + (i as i32) * STEP
}
/// A sample point at the rect's center on frame `i`.
fn rect_center(i: usize) -> (i32, i32) {
    (rect_x(i) + RW / 2, H / 2)
}

struct App {
    tl_surface: WlSurface,
    frames: Vec<WlBuffer>,
    /// Number of frames drawn so far (also the index of the next frame to draw).
    drawn: usize,
    tl_configured: bool,
}

impl App {
    fn draw_frame(&mut self, i: usize, qh: &QueueHandle<App>) {
        self.tl_surface.attach(Some(&self.frames[i]), 0, 0);
        self.tl_surface.damage(0, 0, W, H);
        let _cb: WlCallback = self.tl_surface.frame(qh, ());
        self.tl_surface.commit();
        self.drawn = i + 1;
    }
}

#[test]
fn animation_fluency() {
    let h = Harness::start("animation");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // Pre-render each animation frame: base fill + the rect at its per-frame x.
    let mut frames = Vec::new();
    for i in 0..N {
        let mut buf = solid(W, H, BASE);
        fill_rect(&mut buf, W, H, rect_x(i), RY, RW, RH, RECT);
        frames.push(make_buffer(
            &shm,
            &qh,
            &h.runtime_dir,
            &format!("f{i}"),
            W,
            H,
            &buf,
        ));
    }

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-animation".into());
    tl_surface.commit();

    let mut app = App {
        tl_surface: tl_surface.clone(),
        frames,
        drawn: 0,
        tl_configured: false,
    };

    // Drive the animation to completion purely via frame-callback pacing (each Done draws the next frame).
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.drawn < N {
        assert!(
            Instant::now() < deadline,
            "animation stalled after {} of {N} frames (a frame callback was never released)",
            app.drawn,
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(3));
    }

    // Wait for the LAST frame to actually reach the presenter.
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
            let (cx, cy) = rect_center(N - 1);
            f.width == W && f.pixel_is(cx, cy, RECT)
        })
        .is_some(),
        "final animation frame never composited",
    );

    // ---- assert every frame composited with the rect at its CORRECT, distinct, monotonic position ----
    let mut serials = Vec::new();
    for i in 0..N {
        let (cx, cy) = rect_center(i);
        let frame = h
            .captures
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.width == W && f.height == H && f.pixel_is(cx, cy, RECT))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "animation frame {i} (rect at x={}) never composited",
                    rect_x(i)
                )
            });

        // The rect is at frame i's position and NOT at any other frame's position — a real move, not a
        // stuck or smeared frame.
        assert_eq!(
            frame.pixel(cx, cy).unwrap(),
            RECT,
            "frame {i}: rect at its position"
        );
        for j in 0..N {
            if j != i {
                let (ox, oy) = rect_center(j);
                assert_eq!(
                    frame.pixel(ox, oy).unwrap(),
                    BASE,
                    "frame {i}: no rect at frame {j}'s position"
                );
            }
        }
        save_frame(&format!("animation-frame{i}"), &frame);
        serials.push(frame.serial);
    }

    // Frames were presented in order (the rect advanced monotonically over time, no reordering/stall).
    for i in 1..N {
        assert!(
            serials[i] > serials[i - 1],
            "frame {i} presented after frame {}",
            i - 1
        );
    }
    // Every frame callback was released (the client drew all N frames — pacing did not stall).
    assert_eq!(
        app.drawn, N,
        "all {N} frames drawn via frame-callback pacing"
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
            if !app.tl_configured {
                app.tl_configured = true;
                app.draw_frame(0, qh); // kick off the animation
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
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            // Frame N's callback released -> draw the next frame. This is the pacing seam under test.
            if app.drawn < N {
                app.draw_frame(app.drawn, qh);
            }
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
    XdgToplevel
);
