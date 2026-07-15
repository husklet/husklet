//! DEMO — `subsurface_sync_desync`.
//!
//! Wayland subsurface commit semantics have two modes with OPPOSITE timing, and this demo locks the exact
//! frame each becomes visible:
//!
//!   * SYNC (the default): a subsurface's newly-attached buffer is CACHED and does NOT reach the screen on
//!     the child's own `wl_surface.commit`. It becomes visible only when the PARENT commits (the parent's
//!     commit atomically applies its synchronized children's cached state). So a child commit alone
//!     produces NO new present; the following parent commit is the frame the child first appears in.
//!   * DESYNC (`wl_subsurface.set_desync`): the child's buffer applies on the child's OWN commit — it
//!     appears immediately, with no parent commit required.
//!
//! We drive a real in-process wayland-client: map a toplevel, add one subsurface, and prove the exact
//! present each buffer lands in. A composited PNG is written for the sync-visible and desync-visible
//! states so a human can confirm the child pixels are really there.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::CapturedFrame;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const TL_W: i32 = 200;
const TL_H: i32 = 150;
const TL: [u8; 4] = [0x18, 0x18, 0xB0, 0xFF]; // blue

const C_W: i32 = 40;
const C_H: i32 = 30;
const SYNC_COL: [u8; 4] = [0x10, 0xC8, 0x30, 0xFF]; // green — the buffer attached while SYNC
const DESYNC_COL: [u8; 4] = [0xD0, 0x20, 0x28, 0xFF]; // red — the buffer attached after set_desync
const C_POS: (i32, i32) = (50, 40);

struct App {
    tl_surface: WlSurface,
    tl_buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
}

fn is_child(f: &CapturedFrame, col: [u8; 4]) -> bool {
    f.width == C_W && f.height == C_H && f.pixel_is(C_W / 2, C_H / 2, col)
}
fn child_count(caps: &std::sync::Arc<std::sync::Mutex<Vec<CapturedFrame>>>, col: [u8; 4]) -> usize {
    caps.lock().unwrap().iter().filter(|f| is_child(f, col)).count()
}
fn max_present_serial(caps: &std::sync::Arc<std::sync::Mutex<Vec<CapturedFrame>>>) -> u64 {
    caps.lock().unwrap().iter().map(|f| f.serial).max().unwrap_or(0)
}

#[test]
fn subsurface_sync_desync() {
    let h = Harness::start("subsurface_sync_desync");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let subcompositor: WlSubcompositor = globals.bind(&qh, 1..=1, ()).expect("wl_subcompositor");

    let tl_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "sd-tl", TL_W, TL_H, &solid(TL_W, TL_H, TL));
    let sync_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "sd-sync", C_W, C_H, &solid(C_W, C_H, SYNC_COL));
    let desync_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "sd-desync", C_W, C_H, &solid(C_W, C_H, DESYNC_COL));

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-sync-desync".into());
    tl_surface.commit();

    let mut app = App {
        tl_surface: tl_surface.clone(),
        tl_buffer: tl_buffer.clone(),
        tl_drawn: false,
        tl_frame_done: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == TL_W && f.pixel_is(1, 1, TL)).is_some(),
        "toplevel never composited",
    );

    // ---- subsurface, SYNC by default (no set_desync) ----
    let child = compositor.create_surface(&qh, ());
    let sub: WlSubsurface = subcompositor.get_subsurface(&child, &tl_surface, &qh, ());
    sub.set_position(C_POS.0, C_POS.1);
    // Deliberately DO NOT call set_desync: the subsurface stays synchronized.

    // Attach the green buffer and commit the CHILD only. A synchronized subsurface caches this — it must
    // NOT present until the parent commits.
    child.attach(Some(&sync_buffer), 0, 0);
    child.damage(0, 0, C_W, C_H);
    child.commit();

    // Pump for a bounded window and assert the green child produced NO present of its own.
    let baseline = max_present_serial(&h.captures);
    let watch = Instant::now() + Duration::from_millis(400);
    while Instant::now() < watch {
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        child_count(&h.captures, SYNC_COL),
        0,
        "SYNC subsurface must NOT present on its own commit (before the parent commits)",
    );
    assert_eq!(
        max_present_serial(&h.captures),
        baseline,
        "a sync child commit must not drive any new present",
    );

    // Now commit the PARENT. This is the frame the green child first becomes visible in.
    tl_surface.attach(Some(&tl_buffer), 0, 0);
    tl_surface.damage(0, 0, TL_W, TL_H);
    tl_surface.commit();

    let sync_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| is_child(f, SYNC_COL) && (f.x, f.y) == C_POS)
        .expect("SYNC subsurface never became visible after the parent committed");
    assert_eq!((sync_frame.x, sync_frame.y), C_POS, "sync child exact placement (parent + set_position)");
    assert_eq!(sync_frame.pixel(C_W / 2, C_H / 2).unwrap(), SYNC_COL, "sync child color");
    // It first appeared strictly after the pre-parent-commit baseline.
    assert!(sync_frame.serial > baseline, "sync child present serial ({}) is after the parent commit", sync_frame.serial);

    // Confront: composite the green child over the blue toplevel and write a PNG.
    {
        let tl0 = h.captures.lock().unwrap().iter().rev().find(|f| f.width == TL_W && f.pixel_is(1, 1, TL)).cloned().unwrap();
        save_composited("subsurface_sync_desync-sync-visible", TL_W, TL_H, TL, &[(&tl0, 0, 0), (&sync_frame, C_POS.0, C_POS.1)]);
    }

    // ---- switch to DESYNC: the child's next commit applies on its OWN commit ----
    sub.set_desync();
    // set_desync takes effect at the child's next commit; the position is already applied. Attach the red
    // buffer and commit the CHILD only — NO parent commit this time.
    let before_desync = max_present_serial(&h.captures);
    child.attach(Some(&desync_buffer), 0, 0);
    child.damage(0, 0, C_W, C_H);
    child.commit();

    let desync_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| is_child(f, DESYNC_COL) && (f.x, f.y) == C_POS)
        .expect("DESYNC subsurface never became visible on its own commit (no parent commit was issued)");
    assert_eq!((desync_frame.x, desync_frame.y), C_POS, "desync child exact placement");
    assert_eq!(desync_frame.pixel(C_W / 2, C_H / 2).unwrap(), DESYNC_COL, "desync child color");
    assert!(
        desync_frame.serial > before_desync,
        "desync child presented on its own commit (serial {} > {})",
        desync_frame.serial,
        before_desync,
    );

    // Confront: composite the red child over the blue toplevel and write a PNG.
    {
        let tl1 = h.captures.lock().unwrap().iter().rev().find(|f| f.width == TL_W && f.pixel_is(1, 1, TL)).cloned().unwrap();
        save_composited("subsurface_sync_desync-desync-visible", TL_W, TL_H, TL, &[(&tl1, 0, 0), (&desync_frame, C_POS.0, C_POS.1)]);
    }

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

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
            xdg.ack_configure(serial);
            if !app.tl_drawn {
                app.tl_surface.attach(Some(&app.tl_buffer), 0, 0);
                app.tl_surface.damage(0, 0, TL_W, TL_H);
                let _cb: WlCallback = app.tl_surface.frame(qh, ());
                app.tl_surface.commit();
                app.tl_drawn = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for App {
    fn event(app: &mut Self, _: &WlCallback, e: <WlCallback as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.tl_frame_done = true;
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSubcompositor, WlSubsurface, XdgToplevel);
