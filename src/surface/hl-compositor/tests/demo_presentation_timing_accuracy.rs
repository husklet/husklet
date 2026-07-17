//! DEMO — `presentation_timing_accuracy` (precision battery: `wp_presentation` feedback over 100+ frames).
//!
//! A companion to `demo_presentation_time` that stops at two frames: this one maps a toplevel, binds
//! `wp_presentation`, and drives 100 fully-settled frames (each frame requests feedback, commits fresh
//! damage, and WAITS for that frame's `presented` before committing the next — so exactly one present
//! resolves per frame). It then locks the timing invariants a frame-scheduling client (Chrome, a media
//! player) relies on, PROVABLY exact over the whole run:
//!
//!   * the reported REFRESH interval is byte-constant across every frame — exactly `16_666_666 ns`, the
//!     60 Hz output's interval — with ZERO drift (no per-frame rounding wobble);
//!   * the `vsync` flag is set on every frame;
//!   * the presentation TIMESTAMP is monotonic non-decreasing across all 100 frames (never goes backwards);
//!   * the presentation SEQUENCE is a true frame counter: it starts at 1 and increments by EXACTLY 1 per
//!     presented frame, contiguous with NO gaps and NO duplicates, so `seq[last] - seq[first] == N - 1`
//!     over the run (the deterministic per-frame increment — the modeled clock IS wall-time-driven, so the
//!     timestamps are real host-monotonic readings rather than a synthetic exact-interval cadence, and the
//!     exact per-frame invariant this run pins is the SEQUENCE, not the wall spacing; see the module note).
//!
//! Why the sequence and not the wall spacing: the adapter stamps each present with `CLOCK_MONOTONIC`
//! (`MonotonicClock` = real `Instant` elapsed), so the timestamp spacing tracks how fast the test drives
//! commits, not a fixed 16.6 ms cadence. The DETERMINISTIC, drift-free quantities are therefore (a) the
//! constant refresh the adapter reports and (b) the +1-per-frame sequence — both asserted exactly here.

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
/// 60 Hz interval in ns: `1e12 / 60_000 mHz`, integer-floored. The exact value every frame must report.
const REFRESH_60HZ_NS: u32 = 16_666_666;
/// Fully-settled frames to drive (>= 100 as required). Each is one present, so the run takes ~N refresh
/// intervals of wall time (~1.7 s at 60 Hz) — well within the per-frame 5 s deadline.
const FRAMES: usize = 100;

/// One received `wp_presentation_feedback.presented`, decoded to plain fields.
#[derive(Debug, Clone, Copy)]
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
    presented: Vec<Presented>,
}

#[test]
fn presentation_timing_accuracy() {
    let h = Harness::start("presentation_timing_accuracy");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let _output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
    let presentation: WpPresentation = globals
        .bind(&qh, 1..=2, ())
        .expect("wp_presentation advertised");

    // Two buffers of the same size, alternated per frame so every commit carries genuinely fresh content
    // (a real damage → real present, never coalesced-to-nothing).
    let buf_a = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "pa",
        W,
        H,
        &solid(W, H, [0x30, 0x70, 0xA0, 0xFF]),
    );
    let buf_b = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "pb",
        W,
        H,
        &solid(W, H, [0xA0, 0x40, 0x30, 0xFF]),
    );

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-presentation-timing-accuracy".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buf_a.clone(),
        drawn: false,
        frame_done: false,
        clock_id: None,
        presented: Vec::new(),
    };

    // Map + first configure delivers the clock id and draws the first frame.
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

    // ---- drive N fully-settled frames: one feedback + one present each ----
    for frame in 0..FRAMES {
        let want = frame + 1;
        let buf = if frame % 2 == 0 { &buf_b } else { &buf_a };
        let _fb: WpPresentationFeedback = presentation.feedback(&surface, &qh, ());
        surface.attach(Some(buf), 0, 0);
        surface.damage(0, 0, W, H);
        surface.commit();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.presented.len() < want {
            assert!(
                Instant::now() < deadline,
                "frame {frame}: presented feedback never arrived ({} of {want})",
                app.presented.len()
            );
            let _ = queue.roundtrip(&mut app);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    assert_eq!(
        app.presented.len(),
        FRAMES,
        "exactly {FRAMES} frames presented"
    );

    // ---- lock the invariants over the whole run ----
    for (i, p) in app.presented.iter().enumerate() {
        // Constant, exact refresh: ZERO drift, byte-identical every frame.
        assert_eq!(
            p.refresh_ns, REFRESH_60HZ_NS,
            "frame {i}: refresh is the exact 60 Hz interval (no drift), got {}",
            p.refresh_ns
        );
        assert!(p.vsync, "frame {i}: presented carries the vsync flag");
        assert!(
            p.time_ns > 0,
            "frame {i}: presentation timestamp is nonzero"
        );
    }

    // Monotonic non-decreasing timestamps across every adjacent pair (never goes backwards).
    for i in 1..app.presented.len() {
        let (prev, cur) = (app.presented[i - 1].time_ns, app.presented[i].time_ns);
        assert!(
            cur >= prev,
            "frame {i}: timestamp monotonic, {prev} then {cur}"
        );
    }

    // Sequence is a true frame counter: starts at 1, +1 exactly per frame, contiguous, no gaps/dups.
    assert_eq!(
        app.presented[0].seq, 1,
        "first presented frame is sequence 1"
    );
    for i in 0..app.presented.len() {
        let expect = 1 + i as u64;
        assert_eq!(
            app.presented[i].seq, expect,
            "frame {i}: sequence increments by exactly 1 per presented frame (want {expect}, got {})",
            app.presented[i].seq
        );
    }
    let first = app.presented.first().unwrap().seq;
    let last = app.presented.last().unwrap().seq;
    assert_eq!(
        last - first,
        (FRAMES - 1) as u64,
        "no drift: last seq == first + (N-1) over {FRAMES} frames ({first}..{last})"
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
        if let wp_presentation_feedback::Event::Presented {
            tv_sec_hi,
            tv_sec_lo,
            tv_nsec,
            refresh,
            seq_hi,
            seq_lo,
            flags,
        } = e
        {
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
