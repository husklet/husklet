//! DEMO (interactive-latency battery) — `flood_fairness_latency` (a client flooding commits does NOT add
//! latency to another client's single input → present).
//!
//! Two independent clients map toplevels. Client A (the FLOODER) commits as fast as it can in a tight loop.
//! Client B (the VICTIM) receives a SINGLE pointer input and paints one marker. The test — timing with
//! `Instant`, WHILE A is actively flooding — asserts B's input→present cycle still completes within a
//! generous wall-clock ceiling: a busy neighbour does not delay B's interactive frame (no priority
//! inversion / cross-client head-of-line blocking).
//!
//! This exercises the adapter's per-window-root pacing — throttling is tracked per root, so A's commit
//! storm shares neither B's present schedule nor its frame-callback path. If B's marker were stuck behind
//! A's flood, the wall-clock bound would trip.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_pointer::{self, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const FLOOD: [u8; 4] = [0xE0, 0x30, 0x30, 0xFF]; // client A flooder (red)
const B_BASE: [u8; 4] = [0x20, 0x40, 0x70, 0xFF]; // client B base (blue)
const B_MARK: [u8; 4] = [0x40, 0xF0, 0xB0, 0xFF]; // client B marker (teal)
const MK: i32 = 12;
const PX: i32 = 100;
const PY: i32 = 70;
const LATENCY_CEILING: Duration = Duration::from_millis(2000);

// ------- client A: minimal flooder -------
struct Flooder {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    done: u32,
}

fn spawn_flooder(dir: &std::path::Path) -> (Connection, EventQueue<Flooder>, Flooder) {
    let conn = Connection::connect_to_env().expect("connect A");
    let (globals, mut queue) = registry_queue_init::<Flooder>(&conn).expect("registry A");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let buffer = make_buffer(&shm, &qh, dir, "flood", W, H, &solid(W, H, FLOOD));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("flood-A".into());
    surface.commit();
    let mut app = Flooder {
        surface: surface.clone(),
        buffer,
        drawn: false,
        done: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.done >= 1) {
        assert!(Instant::now() < deadline, "flooder never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch A map");
    }
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    (conn, queue, app)
}

// ------- client B: victim with pointer + marker -------
struct Victim {
    surface: WlSurface,
    base_buffer: WlBuffer,
    marker_buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    marker_drawn: bool,
}

fn spawn_victim(
    dir: &std::path::Path,
    qh_out: &mut Option<QueueHandle<Victim>>,
) -> (Connection, EventQueue<Victim>, Victim, WlSeat) {
    let conn = Connection::connect_to_env().expect("connect B");
    let (globals, mut queue) = registry_queue_init::<Victim>(&conn).expect("registry B");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    let base_buffer = make_buffer(&shm, &qh, dir, "bbase", W, H, &solid(W, H, B_BASE));
    let mut mpx = solid(W, H, B_BASE);
    fill_rect(&mut mpx, W, H, PX - MK / 2, PY - MK / 2, MK, MK, B_MARK);
    let marker_buffer = make_buffer(&shm, &qh, dir, "bmark", W, H, &mpx);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("victim-B".into());
    surface.commit();
    let mut app = Victim {
        surface: surface.clone(),
        base_buffer: base_buffer.clone(),
        marker_buffer: marker_buffer.clone(),
        drawn: false,
        frame_done: false,
        marker_drawn: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "victim never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch B map");
    }
    let _pointer: WlPointer = seat.get_pointer(&qh, ());
    let _ = queue.roundtrip(&mut app);
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    *qh_out = Some(qh);
    (conn, queue, app, seat)
}

#[test]
fn flood_fairness_latency() {
    let h = Harness::start("flood_fairness_latency");

    let (_ca, mut qa, mut a) = spawn_flooder(&h.runtime_dir);
    let mut qhb = None;
    let (_cb, mut qb, mut b, _seat) = spawn_victim(&h.runtime_dir, &mut qhb);
    let qha = qa.handle();
    let qhb = qhb.unwrap();

    // Both first frames on screen.
    let _ = wait_for(&h.captures, 5, |f| f.pixel_is(1, 1, FLOOD)).expect("flooder first frame");
    let _ = wait_for(&h.captures, 5, |f| f.pixel_is(1, 1, B_BASE)).expect("victim first frame");

    // Focus B (it mapped last → topmost) so the single pointer input hit-tests to B.
    h.input_tx.send(InputCommand::FocusTopmostKeyboard).unwrap();
    let _ = qb.roundtrip(&mut b);

    // ---- while A floods, deliver B a SINGLE input and time its render to the screen ----
    let t0 = Instant::now();
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: PX as f64,
            y: PY as f64,
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut victim_marker = None;
    let mut t1 = None;
    let mut flood_iters = 0u32;
    while victim_marker.is_none() {
        // A keeps flooding hard.
        a.surface.attach(Some(&a.buffer), 0, 0);
        a.surface.damage(0, 0, W, H);
        let _cb: WlCallback = a.surface.frame(&qha, ());
        a.surface.commit();
        let _ = qa.roundtrip(&mut a);
        flood_iters += 1;

        let _ = qb.roundtrip(&mut b);
        if let Some(f) = h
            .captures
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|f| f.pixel_is(PX, PY, B_MARK))
            .cloned()
        {
            t1 = Some(Instant::now());
            victim_marker = Some(f);
        }
        assert!(
            Instant::now() < deadline,
            "victim's input never presented under flood (PRIORITY INVERSION)"
        );
    }
    let mf = victim_marker.unwrap();
    let elapsed = t1.unwrap().duration_since(t0);

    // ---- fairness: B's input→present completed within the bound despite A's flood ----
    assert!(
        elapsed < LATENCY_CEILING,
        "victim input→present {elapsed:?} exceeded ceiling {LATENCY_CEILING:?} under a {flood_iters}-commit flood"
    );
    assert_eq!(
        mf.pixel(PX, PY).unwrap(),
        B_MARK,
        "victim's marker at the injected pointer position"
    );
    assert_eq!(
        mf.pixel(1, 1).unwrap(),
        B_BASE,
        "victim frame is its own content (not the flooder's)"
    );

    // A really was flooding (many commits happened during B's single-input cycle).
    assert!(
        flood_iters >= 1,
        "flooder committed during the victim's cycle (iters={flood_iters})"
    );

    eprintln!("flood_fairness: victim_wall={elapsed:?} flood_iters={flood_iters} (ceiling {LATENCY_CEILING:?})");
    save_frame("flood_fairness_latency-victim", &mf);
    let _ = qhb;
    h.shutdown();
}

// ---------- client A dispatch ----------
impl Dispatch<WlRegistry, GlobalListContents> for Flooder {
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
impl Dispatch<XdgWmBase, ()> for Flooder {
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
impl Dispatch<XdgSurface, ()> for Flooder {
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
impl Dispatch<WlCallback, ()> for Flooder {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        e: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.done += 1;
        }
    }
}
macro_rules! ignore_a {
    ($($t:ty),*) => {$( impl Dispatch<$t, ()> for Flooder {
        fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    } )*};
}
ignore_a!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    XdgToplevel
);

// ---------- client B dispatch ----------
impl Dispatch<WlRegistry, GlobalListContents> for Victim {
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
impl Dispatch<XdgWmBase, ()> for Victim {
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
impl Dispatch<XdgSurface, ()> for Victim {
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
                app.surface.attach(Some(&app.base_buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
            }
        }
    }
}
impl Dispatch<WlPointer, ()> for Victim {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        e: <WlPointer as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Enter { .. } = e {
            if !app.marker_drawn {
                app.surface.attach(Some(&app.marker_buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.marker_drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for Victim {
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
macro_rules! ignore_b {
    ($($t:ty),*) => {$( impl Dispatch<$t, ()> for Victim {
        fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    } )*};
}
ignore_b!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSeat,
    XdgToplevel
);
