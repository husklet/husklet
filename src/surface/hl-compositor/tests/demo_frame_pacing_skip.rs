//! DEMO (batch-3) — `frame_pacing_skip` (a no-damage commit skips present; the callback still completes).
//!
//! Interleaves damaged and no-damage commits on one mapped toplevel and locks the damage-skip
//! optimization end to end:
//!
//!   * a DAMAGED commit (attach a new buffer + damage) presents — a new capture with the committed
//!     pixels appears;
//!   * a NO-DAMAGE commit (a bare `wl_surface.frame` + `commit`, no attach, no `damage`) is SKIPPED —
//!     NO new frame is captured — yet its frame callback STILL fires, so the client is paced forward,
//!     not stalled (Skipped pacing completes callbacks without presenting).
//!
//! The exact evidence: after the whole interleave there are EXACTLY 3 presents (the map + two damaged
//! commits), serials `1,2,3`, each the exact committed color — the two no-damage commits contributed
//! zero presents while each still delivered a `wl_callback.done`. That is the redundant-present skip the
//! scene's `is_tree_dirty` short-circuit performs, proven from outside the compositor.

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

const W: i32 = 120;
const H: i32 = 90;
const C0: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF]; // red   — the map frame
const C1: [u8; 4] = [0x20, 0xE0, 0x20, 0xFF]; // green — first damaged commit
const C2: [u8; 4] = [0x20, 0x20, 0xE0, 0xFF]; // blue  — second damaged commit

struct App {
    surface: WlSurface,
    base_buffer: WlBuffer,
    drawn: bool,
    /// Count of `wl_callback.done` events received — a monotonic pacing tick the test waits on.
    dones: u32,
    /// Set once the initial map's frame callback has fired (map is complete).
    mapped_done: bool,
}

impl App {
    /// A bare frame-callback-only commit: request a frame callback, then commit with NO attach + NO
    /// damage. Nothing visible changed, so the compositor must SKIP the present but still fire the callback.
    fn no_damage_commit(&mut self, qh: &QueueHandle<App>) {
        let _cb: WlCallback = self.surface.frame(qh, ());
        self.surface.commit();
    }
}

#[test]
fn frame_pacing_skip() {
    let h = Harness::start("frame_pacing_skip");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let base_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "c0", W, H, &solid(W, H, C0));
    let buf1 = make_buffer(&shm, &qh, &h.runtime_dir, "c1", W, H, &solid(W, H, C1));
    let buf2 = make_buffer(&shm, &qh, &h.runtime_dir, "c2", W, H, &solid(W, H, C2));

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-pacing".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        base_buffer: base_buffer.clone(),
        drawn: false,
        dones: 0,
        mapped_done: false,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.mapped_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // ---- present #1: the map frame (C0). Exactly one capture so far. ----
    let f0 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, C0)
    })
    .expect("map (C0) frame never composited");
    assert_eq!(f0.serial, 1, "map is present #1");
    assert_eq!(
        h.captures.lock().unwrap().len(),
        1,
        "one present after the map"
    );

    // ---- no-damage commit #1: must SKIP present but still fire the callback ----
    let before = app.dones;
    app.no_damage_commit(&qh);
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.dones > before),
        "no-damage commit #1 never fired its frame callback (client stalled?)",
    );
    assert_eq!(
        h.captures.lock().unwrap().len(),
        1,
        "no-damage commit #1 produced NO new present (redundant-present skip)",
    );

    // ---- present #2: first damaged commit (C1) ----
    app.surface.attach(Some(&buf1), 0, 0);
    app.surface.damage(0, 0, W, H);
    let _cb1: WlCallback = app.surface.frame(&qh, ());
    app.surface.commit();
    let f1 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(1, 1, C1)
    })
    .expect("damaged commit (C1) never presented");
    assert_eq!(f1.serial, 2, "first damaged commit is present #2");
    assert_eq!(
        h.captures.lock().unwrap().len(),
        2,
        "two presents after the first damaged commit"
    );

    // ---- no-damage commit #2: skip again ----
    let before = app.dones;
    app.no_damage_commit(&qh);
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.dones > before),
        "no-damage commit #2 never fired its frame callback (client stalled?)",
    );
    assert_eq!(
        h.captures.lock().unwrap().len(),
        2,
        "no-damage commit #2 produced NO new present (redundant-present skip)",
    );

    // ---- present #3: second damaged commit (C2) ----
    app.surface.attach(Some(&buf2), 0, 0);
    app.surface.damage(0, 0, W, H);
    let _cb2: WlCallback = app.surface.frame(&qh, ());
    app.surface.commit();
    let f2 = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(1, 1, C2)
    })
    .expect("damaged commit (C2) never presented");
    assert_eq!(f2.serial, 3, "second damaged commit is present #3");

    // ---- whole-run: exactly 3 presents, dense serials, exact ordered colors ----
    let caps = h.captures.lock().unwrap().clone();
    assert_eq!(
        caps.len(),
        3,
        "exactly 3 presents (map + 2 damaged); the 2 no-damage commits skipped"
    );
    assert_eq!(
        caps.iter().map(|f| f.serial).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "dense monotonic serials"
    );
    assert_eq!(caps[0].pixel(W / 2, H / 2).unwrap(), C0, "present #1 is C0");
    assert_eq!(caps[1].pixel(W / 2, H / 2).unwrap(), C1, "present #2 is C1");
    assert_eq!(caps[2].pixel(W / 2, H / 2).unwrap(), C2, "present #3 is C2");

    save_frame("frame_pacing_skip-1_map_C0", &caps[0]);
    save_frame("frame_pacing_skip-2_damaged_C1", &caps[1]);
    save_frame("frame_pacing_skip-3_damaged_C2", &caps[2]);

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
    fn event(
        app: &mut Self,
        _: &WlCallback,
        e: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.dones += 1;
            app.mapped_done = true;
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
