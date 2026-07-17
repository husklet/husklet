//! ROBUSTNESS DEMO 4 — `commit_flood_fairness` (a flooding client cannot starve a well-behaved one).
//!
//! Two independent clients map toplevels. The FLOODER (client A) commits as fast as it can in a tight
//! loop — attach + full-surface damage + frame callback + commit, hundreds of times. The NORMAL client
//! (client B) commits at a modest cadence (once per several flood iterations). The test asserts B makes
//! real FORWARD PROGRESS while A floods: B's `wl_surface.frame` callbacks keep completing (a callback
//! fires only once B's committed frame actually reaches the presenter) AND B's pixels keep landing in the
//! capture log as fresh frames. A starved B would see its callbacks stall and its frames dry up.
//!
//! This exercises the adapter's per-root pacing: throttling/coalescing is tracked per window root, so one
//! surface spamming commits must not monopolize the present path and block another surface's frames.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 120;
const H: i32 = 90;
const FLOOD: [u8; 4] = [0xE0, 0x30, 0x30, 0xFF]; // red flooder
const NORMAL: [u8; 4] = [0x30, 0x60, 0xE0, 0xFF]; // blue well-behaved client
const ITERS: usize = 400; // flood iterations
const B_EVERY: usize = 8; // client B commits once per this many flood iterations

struct Client {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    done: u32,
}

fn spawn(
    dir: &std::path::Path,
    tag: &str,
    color: [u8; 4],
) -> (Connection, EventQueue<Client>, Client) {
    let conn = Connection::connect_to_env().expect("connect");
    let (globals, mut queue) = registry_queue_init::<Client>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let buffer = make_buffer(&shm, &qh, dir, tag, W, H, &solid(W, H, color));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("flood-{tag}"));
    surface.commit();
    let mut app = Client {
        surface: surface.clone(),
        buffer,
        drawn: false,
        done: 0,
    };
    // Wait until the first frame's callback has fired — that only happens once the frame actually
    // reached the presenter, so the client is fully mapped AND its first frame is captured.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.done >= 1) {
        assert!(
            Instant::now() < deadline,
            "client {tag} never mapped/presented its first frame"
        );
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    (conn, queue, app)
}

/// One commit cycle: re-attach, damage the whole surface, request a frame callback, commit.
fn commit_frame(app: &Client, qh: &QueueHandle<Client>) {
    app.surface.attach(Some(&app.buffer), 0, 0);
    app.surface.damage(0, 0, W, H);
    let _cb: WlCallback = app.surface.frame(qh, ());
    app.surface.commit();
}

#[test]
fn commit_flood_fairness() {
    let h = Harness::start("commit_flood");

    let (_ca, mut qa, mut a) = spawn(&h.runtime_dir, "flood", FLOOD);
    let (_cb, mut qb, mut b) = spawn(&h.runtime_dir, "normal", NORMAL);
    let qha = qa.handle();
    let qhb = qb.handle();

    // Both mapped their first frame.
    let _ = wait_for(&h.captures, 5, |f| f.pixel_is(1, 1, FLOOD)).expect("flooder first frame");
    let normal_first =
        wait_for(&h.captures, 5, |f| f.pixel_is(1, 1, NORMAL)).expect("normal first frame");

    // Baseline: how many NORMAL-colored frames existed before the flood; count new ones after.
    let normal_before = h
        .captures
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f.pixel_is(1, 1, NORMAL))
        .count();
    let b_done_before = b.done;

    // ---- the flood: A commits every iteration, B every B_EVERY iterations ----
    for i in 0..ITERS {
        commit_frame(&a, &qha);
        if i % B_EVERY == 0 {
            commit_frame(&b, &qhb);
        }
        let _ = qa.roundtrip(&mut a);
        let _ = qb.roundtrip(&mut b);
    }

    // Drain remaining callbacks/presents for a moment.
    let settle = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settle {
        let _ = qa.roundtrip(&mut a);
        let _ = qb.roundtrip(&mut b);
        std::thread::sleep(Duration::from_millis(5));
    }

    // ---- fairness assertions: B was NOT starved ----
    let b_done_after = b.done;
    let b_frames_completed = b_done_after - b_done_before;
    let normal_after = h
        .captures
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f.pixel_is(1, 1, NORMAL))
        .count();
    let normal_new = normal_after - normal_before;

    // B committed ITERS / B_EVERY times (~50). It must have made real forward progress: many of its
    // frame callbacks completed AND many fresh NORMAL frames reached the presenter. A starved B would be
    // near zero on both.
    let committed = ITERS / B_EVERY;
    assert!(
        b_frames_completed as usize >= committed / 3,
        "normal client starved: only {b_frames_completed} of ~{committed} frame callbacks completed under flood"
    );
    assert!(
        normal_new >= committed / 3,
        "normal client's frames dried up under flood: only {normal_new} new NORMAL frames (of ~{committed} commits)"
    );

    // And it is still the right pixels (no corruption from the flood interleave).
    let latest_normal =
        wait_for(&h.captures, 2, |f| f.pixel_is(1, 1, NORMAL)).expect("a NORMAL frame is present");
    assert_eq!(
        latest_normal.pixel(W / 2, H / 2).unwrap(),
        NORMAL,
        "normal client's frame is solid blue"
    );

    save_composited(
        "commit_flood-fairness",
        2 * W + 20,
        H,
        [0x10, 0x10, 0x10, 0xFF],
        &[(&normal_first, 0, 0), (&latest_normal, W + 20, 0)],
    );

    eprintln!("fairness: B completed {b_frames_completed} frame callbacks and {normal_new} new frames under a {ITERS}-commit flood");

    h.shutdown();
}

// ---------- dispatch plumbing ----------
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
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
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
            app.done += 1;
        }
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
    XdgToplevel
);
