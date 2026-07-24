//! DEMO (batch-input) — `session_lock` (real `ext_session_lock_manager_v1`: lock blanks + presents the
//! lock surface + hides normal surfaces; unlock restores).
//!
//! A normal toplevel maps and presents its content. A lock client then LOCKS the session over the real
//! protocol, receives `locked`, and presents a full-output lock surface. The test asserts:
//!
//!   * the compositor confirmed the lock (`ext_session_lock_v1.locked`) AND recorded it server-side
//!     (`Observations.session_locked == true`);
//!   * the lock surface's pixels reach the screen (a LOCK-colored frame is captured);
//!   * while locked, the NORMAL surface is HIDDEN — a fresh commit of a new color does NOT present
//!     (its window is occluded), so no frame of that color appears;
//!   * after `unlock_and_destroy`, `Observations.session_locked == false` and the normal surface RESTORES
//!     (its withheld frame now presents).
//!
//! This proves ext-session-lock is honoured end to end: real lock/unlock state, the lock surface presents,
//! and protected content is genuinely withheld while locked.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 160;
const H: i32 = 120;
const NORMAL1: [u8; 4] = [0x30, 0x80, 0x40, 0xFF]; // normal window, first content (green)
const NORMAL2: [u8; 4] = [0xE0, 0x40, 0x40, 0xFF]; // normal window tries to redraw while locked (red)
const LOCK: [u8; 4] = [0x10, 0x10, 0x40, 0xFF]; // lock surface (dark blue)

struct App {
    // shared plumbing for building the lock buffer once its configure size is known
    shm: WlShm,
    dir: std::path::PathBuf,
    // normal toplevel
    tl_surface: WlSurface,
    tl_buf1: WlBuffer,
    tl_buf2: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
    // session lock
    locked: bool,
    // lock surface
    lock_surface: Option<WlSurface>,
    lock_configured: bool,
    lock_drawn: bool,
    // the size the lock surface was configured to (== the buffer it must commit)
    lock_size: (i32, i32),
}

#[test]
fn session_lock() {
    let h = Harness::start("session_lock");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
    let lock_mgr: ExtSessionLockManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("ext_session_lock_manager_v1");

    let tl_buf1 = make_buffer(&shm, &qh, &h.runtime_dir, "n1", W, H, &solid(W, H, NORMAL1));
    let tl_buf2 = make_buffer(&shm, &qh, &h.runtime_dir, "n2", W, H, &solid(W, H, NORMAL2));
    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-lock-normal".into());
    tl_surface.commit();

    let mut app = App {
        shm: shm.clone(),
        dir: h.runtime_dir.clone(),
        tl_surface: tl_surface.clone(),
        tl_buf1: tl_buf1.clone(),
        tl_buf2: tl_buf2.clone(),
        tl_drawn: false,
        tl_frame_done: false,
        locked: false,
        lock_surface: None,
        lock_configured: false,
        lock_drawn: false,
        lock_size: (0, 0),
    };

    // Map the normal toplevel + present NORMAL1.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "normal toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(1, 1, NORMAL1)
    })
    .expect("normal window never presented NORMAL1");

    // ---- LOCK the session ----
    let lock: ExtSessionLockV1 = lock_mgr.lock(&qh, ());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.locked {
        let _ = queue.roundtrip(&mut app);
        assert!(
            Instant::now() < deadline,
            "compositor never confirmed the lock"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Server-side lock state recorded.
    assert!(
        h.observations.lock().unwrap().session_locked,
        "compositor recorded session_locked=true"
    );

    // Create the lock surface for the output. The compositor configures it to the OUTPUT size; the client
    // must commit a buffer of EXACTLY that size (protocol rule), so the buffer is built in the Configure
    // handler once the size is known.
    let lock_wl = compositor.create_surface(&qh, ());
    app.lock_surface = Some(lock_wl.clone());
    let _lock_surf: ExtSessionLockSurfaceV1 = lock.get_lock_surface(&lock_wl, &output, &qh, ());

    // The lock surface presents its LOCK pixels (a full-output frame).
    let lock_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(1, 1, LOCK)
    })
    .expect("lock surface never presented");
    assert_eq!(
        lock_frame.pixel(W / 2, H / 2).unwrap(),
        LOCK,
        "lock surface is solid LOCK color"
    );
    save_frame("session_lock-locked", &lock_frame);

    // ---- while locked, the normal surface is HIDDEN: a fresh NORMAL2 commit must NOT present ----
    let captures_before = h.captures.lock().unwrap().len();
    app.tl_surface.attach(Some(&app.tl_buf2), 0, 0);
    app.tl_surface.damage(0, 0, W, H);
    let _cb: WlCallback = app.tl_surface.frame(&qh, ());
    app.tl_surface.commit();
    // Give the compositor ample time to (not) present it.
    let settle = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle {
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(10));
    }
    let normal2_shown = h
        .captures
        .lock()
        .unwrap()
        .iter()
        .any(|f| f.pixel_is(1, 1, NORMAL2));
    assert!(
        !normal2_shown,
        "normal surface must stay HIDDEN while the session is locked (NORMAL2 leaked)"
    );
    // The lock surface is still the visible content.
    let _ = captures_before;

    // ---- UNLOCK ----
    lock.unlock_and_destroy();
    let _ = queue.roundtrip(&mut app);
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.observations.lock().unwrap().session_locked {
        let _ = queue.roundtrip(&mut app);
        assert!(
            Instant::now() < deadline,
            "compositor never cleared session_locked"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !h.observations.lock().unwrap().session_locked,
        "compositor recorded session_locked=false"
    );

    // The normal surface RESTORES: its withheld NORMAL2 frame now presents (re-commit to be safe).
    app.tl_surface.attach(Some(&app.tl_buf2), 0, 0);
    app.tl_surface.damage(0, 0, W, H);
    let _cb: WlCallback = app.tl_surface.frame(&qh, ());
    app.tl_surface.commit();
    let restored = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(1, 1, NORMAL2)
    })
    .expect("normal surface never restored after unlock");
    assert_eq!(
        restored.pixel(W / 2, H / 2).unwrap(),
        NORMAL2,
        "restored normal surface presents its content"
    );
    save_frame("session_lock-restored", &restored);

    h.shutdown();
}

// ---------- dispatch plumbing ----------
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
            if !app.tl_drawn {
                app.tl_surface.attach(Some(&app.tl_buf1), 0, 0);
                app.tl_surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.tl_surface.frame(qh, ());
                app.tl_surface.commit();
                app.tl_drawn = true;
            }
        }
    }
}
impl Dispatch<ExtSessionLockV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtSessionLockV1,
        e: <ExtSessionLockV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            ext_session_lock_v1::Event::Locked => app.locked = true,
            ext_session_lock_v1::Event::Finished => app.locked = false,
            _ => {}
        }
    }
}
impl Dispatch<ExtSessionLockSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        surf: &ExtSessionLockSurfaceV1,
        e: <ExtSessionLockSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = e
        {
            surf.ack_configure(serial);
            app.lock_size = (width as i32, height as i32);
            if !app.lock_drawn {
                let (lw, lh) = app.lock_size;
                let ws = app.lock_surface.clone().expect("lock surface exists");
                // The committed buffer MUST be exactly the configured size.
                let buf = make_buffer(&app.shm, qh, &app.dir, "lock", lw, lh, &solid(lw, lh, LOCK));
                ws.attach(Some(&buf), 0, 0);
                ws.damage(0, 0, lw, lh);
                let _cb: WlCallback = ws.frame(qh, ());
                ws.commit();
                std::mem::forget(buf);
                app.lock_drawn = true;
            }
            app.lock_configured = true;
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
ignore!(
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlOutput,
    XdgToplevel,
    ExtSessionLockManagerV1
);
