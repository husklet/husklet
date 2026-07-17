//! DEMO — `presentation_time` (`wp_presentation`: a committed frame reports when it hit the screen).
//!
//! A client maps a toplevel and binds `wp_presentation`. For each of TWO frames it requests
//! `wp_presentation.feedback(surface)` and commits new content; it asserts the compositor answers
//! `wp_presentation_feedback.presented` with SANE fields:
//!
//!   * a nonzero, MONOTONIC presentation timestamp (the second frame's is >= the first's) in the
//!     `CLOCK_MONOTONIC` timeline `wp_presentation.clock_id` advertised;
//!   * a strictly INCREASING presentation sequence (`seq` frame 2 > frame 1);
//!   * a nonzero refresh interval (~16.6 ms for the 60 Hz output);
//!   * the `vsync` flag set.
//!
//! It also proves the `discarded` path: a feedback requested on a surface that is then destroyed without
//! its content ever being shown is answered `discarded` (never `presented`).
//!
//! Proves the adapter's newly-wired presentation-time global delivers real present-timing feedback — what a
//! media player / compositor-throttled client uses to schedule the next frame.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, Kind, WpPresentationFeedback},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;

/// One received `wp_presentation_feedback.presented`, decoded to plain fields.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Presented {
    time_ns: u128,
    refresh_ns: u32,
    seq: u64,
    vsync: bool,
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    clock_id: Option<u32>,
    /// Every `presented` feedback, in order.
    presented: Vec<Presented>,
    /// Set when a `discarded` feedback arrives.
    discarded: bool,
    sync_outputs: u32,
}

#[test]
fn presentation_time() {
    let h = Harness::start("presentation_time");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // Bind the output so the compositor can send `sync_output` before `presented`.
    let _output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
    // The newly-wired global under test (version 2 → sends clock_id on bind).
    let presentation: WpPresentation = globals
        .bind(&qh, 1..=2, ())
        .expect("wp_presentation advertised");

    let color = [0x30u8, 0x70, 0xA0, 0xFF];
    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "pt", W, H, &solid(W, H, color));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-presentation-time".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
        clock_id: None,
        presented: Vec::new(),
        discarded: false,
        sync_outputs: 0,
    };

    // Bind + first configure delivers the clock id and maps the surface (the configure handler attaches
    // the buffer and commits the first frame).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = queue.roundtrip(&mut app);
    assert_eq!(
        app.clock_id,
        Some(1),
        "wp_presentation advertised CLOCK_MONOTONIC (clk id 1)"
    );

    // ---- two frames, each with a presentation-feedback request ----
    for frame in 0..2 {
        let want = frame + 1;
        // Request feedback for THIS content update, then commit new content (a fresh damage forces a
        // present so the feedback resolves rather than coalescing to nothing).
        let _fb: WpPresentationFeedback = presentation.feedback(&surface, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, W, H);
        surface.commit();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.presented.len() < want {
            assert!(
                Instant::now() < deadline,
                "frame {frame}: presented feedback never arrived, got {:?}",
                app.presented
            );
            let _ = queue.roundtrip(&mut app);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let p0 = app.presented[0];
    let p1 = app.presented[1];

    // Sane timestamp: nonzero + monotonic across frames.
    assert!(
        p0.time_ns > 0,
        "frame 0 presentation timestamp is nonzero, got {p0:?}"
    );
    assert!(
        p1.time_ns >= p0.time_ns,
        "presentation timestamp is monotonic: {} then {}",
        p0.time_ns,
        p1.time_ns
    );
    // Strictly increasing sequence.
    assert!(
        p0.seq >= 1,
        "frame 0 sequence is a real frame counter (>=1), got {}",
        p0.seq
    );
    assert!(
        p1.seq > p0.seq,
        "presentation sequence strictly increases: {} then {}",
        p0.seq,
        p1.seq
    );
    // Nonzero refresh (~16.67 ms for 60 Hz) + vsync flag.
    assert!(
        p0.refresh_ns > 0,
        "refresh interval is nonzero, got {}",
        p0.refresh_ns
    );
    assert_eq!(
        p0.refresh_ns, 16_666_666,
        "refresh matches the 60 Hz output interval, got {}",
        p0.refresh_ns
    );
    assert!(p0.vsync, "presented carries the vsync flag");

    // ---- discarded path: feedback on a surface torn down without its content ever being shown ----
    // A bare wl_surface (no role, no buffer) requests feedback and commits, then is destroyed. Its content
    // never reaches the screen, so the compositor must answer `discarded`, never `presented`.
    let gone = compositor.create_surface(&qh, ());
    let _fb_gone: WpPresentationFeedback = presentation.feedback(&gone, &qh, ());
    gone.commit();
    let _ = queue.roundtrip(&mut app);
    gone.destroy();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.discarded {
        assert!(
            Instant::now() < deadline,
            "feedback on a never-shown, destroyed surface was never discarded"
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        app.discarded,
        "a never-shown surface's feedback was discarded (not falsely presented)"
    );
    // Each `presented` was preceded by a `sync_output` naming the output the surface was shown on (the
    // client bound the single `wl_output`), so it learns WHERE its frame landed — one per presented frame.
    assert_eq!(
        app.sync_outputs,
        app.presented.len() as u32,
        "one sync_output per presented frame, got {} for {} frames",
        app.sync_outputs,
        app.presented.len()
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
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
            // Only the primary surface draws its first frame here. The discard-path surface is created
            // AFTER the primary maps (`drawn` already true), so this guard leaves it intentionally
            // buffer-less — its later feedback is answered `discarded`, never `presented`.
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
impl Dispatch<WpPresentation, ()> for App {
    fn event(
        app: &mut Self,
        _: &WpPresentation,
        e: <WpPresentation as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = e {
            app.clock_id = Some(clk_id);
        }
    }
}
impl Dispatch<WpPresentationFeedback, ()> for App {
    fn event(
        app: &mut Self,
        _: &WpPresentationFeedback,
        e: <WpPresentationFeedback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wp_presentation_feedback::Event::SyncOutput { .. } => app.sync_outputs += 1,
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => {
                let secs = ((tv_sec_hi as u64) << 32) | tv_sec_lo as u64;
                let time_ns = secs as u128 * 1_000_000_000u128 + tv_nsec as u128;
                let seq = ((seq_hi as u64) << 32) | seq_lo as u64;
                let vsync = matches!(flags, WEnum::Value(k) if k.contains(Kind::Vsync));
                app.presented.push(Presented {
                    time_ns,
                    refresh_ns: refresh,
                    seq,
                    vsync,
                });
            }
            wp_presentation_feedback::Event::Discarded => app.discarded = true,
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
    WlOutput,
    XdgToplevel
);
