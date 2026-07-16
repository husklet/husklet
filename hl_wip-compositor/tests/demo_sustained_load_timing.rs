//! DEMO — `sustained_load_timing` (precision battery: presentation feedback stays exact under a flood).
//!
//! Where `demo_presentation_timing_accuracy` drives fully-settled one-present-per-frame steps, this demo
//! FLOODS the compositor: it fires 150 `wp_presentation.feedback` requests + fresh-damage commits in tight
//! bursts (many commits per refresh interval), so the vsync throttle COALESCES bursts and several feedbacks
//! release together on one present. It then proves the presentation feedback timing stays accurate under
//! that load:
//!
//!   * every feedback is eventually answered `presented` (none lost, none stuck) — all 150 resolve;
//!   * the reported refresh is the exact 60 Hz interval on every one, and the vsync flag is set;
//!   * timestamps are monotonic non-decreasing in delivery order (no backwards jitter under load);
//!   * the SEQUENCE is a true per-vblank frame counter: the DISTINCT sequence numbers form a contiguous
//!     run starting at 1 with NO gaps, and — the key correctness lock — every feedback released by the SAME
//!     present carries the SAME sequence AND the SAME timestamp. Two feedbacks answered at one identical
//!     instant MUST share one sequence number (one vblank = one number); giving them distinct sequences
//!     would report several vblanks at a single instant, which no display can produce. This is exactly the
//!     coalesced-burst case a flood forces, so this demo is the regression guard for that invariant.

mod common;
use common::*;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_output::WlOutput, wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
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
const REFRESH_60HZ_NS: u32 = 16_666_666;
/// Total feedback+commit pairs to flood, and the burst size flushed together so a burst lands inside one
/// refresh interval and coalesces (forcing several feedbacks onto one present).
const FLOOD: usize = 150;
const BURST: usize = 10;

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
    presented: Vec<Presented>,
    discarded: u32,
}

#[test]
fn sustained_load_timing() {
    let h = Harness::start("sustained_load_timing");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let _output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
    let presentation: WpPresentation = globals.bind(&qh, 1..=2, ()).expect("wp_presentation advertised");

    let buf_a = make_buffer(&shm, &qh, &h.runtime_dir, "fa", W, H, &solid(W, H, [0x20, 0x60, 0xC0, 0xFF]));
    let buf_b = make_buffer(&shm, &qh, &h.runtime_dir, "fb", W, H, &solid(W, H, [0xC0, 0x50, 0x20, 0xFF]));

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-sustained-load".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buf_a.clone(),
        drawn: false,
        frame_done: false,
        presented: Vec::new(),
        discarded: 0,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = queue.roundtrip(&mut app);

    // ---- flood: FLOOD feedback+commit pairs in bursts of BURST, flushed together so they coalesce ----
    for i in 0..FLOOD {
        let buf = if i % 2 == 0 { &buf_b } else { &buf_a };
        let _fb: WpPresentationFeedback = presentation.feedback(&surface, &qh, ());
        surface.attach(Some(buf), 0, 0);
        surface.damage(0, 0, W, H);
        surface.commit();
        if (i + 1) % BURST == 0 {
            // Flush the whole burst at once (it lands inside one refresh interval → the throttle coalesces
            // it) and drain whatever feedback has already resolved.
            let _ = queue.roundtrip(&mut app);
        }
    }

    // ---- drain: pump until every flooded feedback is answered (presented, under load) ----
    let deadline = Instant::now() + Duration::from_secs(30);
    while (app.presented.len() + app.discarded as usize) < FLOOD {
        assert!(
            Instant::now() < deadline,
            "under load only {} of {FLOOD} feedbacks resolved",
            app.presented.len()
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(app.discarded, 0, "no feedback was discarded under load (all reached the screen)");
    assert_eq!(app.presented.len(), FLOOD, "all {FLOOD} flooded feedbacks were presented");

    // Per-frame field invariants: exact refresh, vsync set, nonzero timestamp.
    for (i, p) in app.presented.iter().enumerate() {
        assert_eq!(p.refresh_ns, REFRESH_60HZ_NS, "feedback {i}: exact 60 Hz refresh, got {}", p.refresh_ns);
        assert!(p.vsync, "feedback {i}: vsync flag set under load");
        assert!(p.time_ns > 0, "feedback {i}: nonzero timestamp");
    }

    // Timestamps monotonic non-decreasing in delivery order (no backwards jitter under load).
    for i in 1..app.presented.len() {
        assert!(
            app.presented[i].time_ns >= app.presented[i - 1].time_ns,
            "feedback {i}: timestamp monotonic under load, {} then {}",
            app.presented[i - 1].time_ns,
            app.presented[i].time_ns
        );
    }
    // Sequence monotonic non-decreasing in delivery order (coalesced feedbacks repeat a seq, never drop).
    for i in 1..app.presented.len() {
        assert!(
            app.presented[i].seq >= app.presented[i - 1].seq,
            "feedback {i}: sequence never decreases, {} then {}",
            app.presented[i - 1].seq,
            app.presented[i].seq
        );
    }

    // Distinct sequence numbers form a contiguous run starting at 1 — one number per actual present, no gaps.
    let mut distinct: Vec<u64> = app.presented.iter().map(|p| p.seq).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(*distinct.first().unwrap(), 1, "distinct sequences start at 1");
    for (k, &s) in distinct.iter().enumerate() {
        assert_eq!(s, 1 + k as u64, "distinct sequences are contiguous (no gaps): want {}, got {s}", 1 + k as u64);
    }

    // KEY LOCK: every feedback answered at one identical timestamp shares ONE sequence number (one vblank =
    // one number), and — symmetrically — one sequence maps to one timestamp. A coalesced burst is exactly
    // where a per-feedback counter would (wrongly) emit several sequences at a single instant.
    let mut seq_by_time: BTreeMap<u128, u64> = BTreeMap::new();
    let mut time_by_seq: BTreeMap<u64, u128> = BTreeMap::new();
    for p in &app.presented {
        if let Some(&s) = seq_by_time.get(&p.time_ns) {
            assert_eq!(s, p.seq, "two feedbacks at the same instant {} carry different sequences ({s} vs {})", p.time_ns, p.seq);
        } else {
            seq_by_time.insert(p.time_ns, p.seq);
        }
        if let Some(&t) = time_by_seq.get(&p.seq) {
            assert_eq!(t, p.time_ns, "sequence {} reported at two different timestamps ({t} vs {})", p.seq, p.time_ns);
        } else {
            time_by_seq.insert(p.seq, p.time_ns);
        }
    }

    // The flood really did coalesce: fewer distinct presents than feedbacks, so the shared-seq path above
    // was genuinely exercised (not a trivially-satisfied one-feedback-per-present run).
    assert!(
        distinct.len() < FLOOD,
        "flood coalesced: {} distinct presents for {FLOOD} feedbacks (multi-feedback presents exercised)",
        distinct.len()
    );

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
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
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e { app.frame_done = true; }
    }
}
impl Dispatch<WpPresentation, ()> for App {
    fn event(_: &mut Self, _: &WpPresentation, _: <WpPresentation as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpPresentationFeedback, ()> for App {
    fn event(app: &mut Self, _: &WpPresentationFeedback, e: <WpPresentationFeedback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi, tv_sec_lo, tv_nsec, refresh, seq_hi, seq_lo, flags,
            } => {
                let secs = ((tv_sec_hi as u64) << 32) | tv_sec_lo as u64;
                let time_ns = secs as u128 * 1_000_000_000u128 + tv_nsec as u128;
                let seq = ((seq_hi as u64) << 32) | seq_lo as u64;
                let vsync = matches!(flags, WEnum::Value(k) if k.contains(Kind::Vsync));
                app.presented.push(Presented { time_ns, refresh_ns: refresh, seq, vsync });
            }
            wp_presentation_feedback::Event::Discarded => app.discarded += 1,
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlOutput, XdgToplevel);
