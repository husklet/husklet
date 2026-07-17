//! DEMO — `output_enter_leave`.
//!
//! `wl_surface.enter` / `wl_surface.leave` tell a client which `wl_output`(s) currently display its
//! surface — the signal GTK/Chrome read to choose a render scale. This demo drives a real in-process
//! wayland-client that binds the advertised `wl_output`, then:
//!
//!   * maps a toplevel and asserts it receives `wl_surface.enter` naming EXACTLY that `wl_output`;
//!   * unmaps the surface (`attach(null)` + commit) and asserts a matching `wl_surface.leave` for the
//!     same `wl_output`;
//!   * re-maps and asserts a second `enter` for the same output.
//!
//! The headless compositor advertises a single `wl_output`, so surface→output membership tracks map/unmap
//! (a mapped toplevel is on the output; an unmapped one is not). Position-based multi-output routing is a
//! documented gap (there is one output), so this locks the map/unmap enter/leave transitions exactly.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_output::WlOutput,
    wl_registry::WlRegistry,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::{self, WlSurface},
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 100;
const H: i32 = 80;
const COL: [u8; 4] = [0x30, 0xA0, 0xE0, 0xFF];

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
    /// `wl_surface.enter` outputs, in arrival order.
    enters: Vec<ObjectId>,
    /// `wl_surface.leave` outputs, in arrival order.
    leaves: Vec<ObjectId>,
}

#[test]
fn output_enter_leave() {
    let h = Harness::start("output_enter_leave");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    // Bind the advertised wl_output BEFORE mapping, so its enter/leave events target our instance.
    let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
    let output_id = output.id();

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "oel", W, H, &solid(W, H, COL));

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-output-enter-leave".into());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        tl_drawn: false,
        tl_frame_done: false,
        enters: Vec::new(),
        leaves: Vec::new(),
    };

    // ---- map: expect exactly one enter naming our wl_output ----
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let mapped = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(W / 2, H / 2, COL)
    })
    .expect("mapped content never composited");
    save_frame("output_enter_leave-mapped", &mapped);

    // Pump until the enter arrives.
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.enters.is_empty() {
        assert!(
            Instant::now() < deadline,
            "no wl_surface.enter after mapping"
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.enters,
        vec![output_id.clone()],
        "map delivers exactly one enter naming the advertised wl_output"
    );
    assert!(app.leaves.is_empty(), "no leave yet");

    // ---- unmap: attach a null buffer + commit; expect a matching leave for the same output ----
    app.surface.attach(None, 0, 0);
    app.surface.commit();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.leaves.is_empty() {
        assert!(
            Instant::now() < deadline,
            "no wl_surface.leave after unmapping"
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.leaves,
        vec![output_id.clone()],
        "unmap delivers exactly one leave for the same wl_output"
    );
    assert_eq!(app.enters.len(), 1, "unmap does not spuriously re-enter");

    // ---- re-map: attach the buffer again + commit; expect a second enter for the same output ----
    app.surface.attach(Some(&app.buffer), 0, 0);
    app.surface.damage(0, 0, W, H);
    app.surface.commit();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.enters.len() < 2 {
        assert!(
            Instant::now() < deadline,
            "no second wl_surface.enter after re-mapping"
        );
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.enters,
        vec![output_id.clone(), output_id.clone()],
        "re-map delivers a second enter for the same output"
    );
    assert_eq!(
        app.leaves.len(),
        1,
        "exactly one leave over the whole cycle"
    );

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlSurface, ()> for App {
    fn event(
        app: &mut Self,
        s: &WlSurface,
        e: <WlSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let _ = s;
        match e {
            wl_surface::Event::Enter { output } => app.enters.push(output.id()),
            wl_surface::Event::Leave { output } => app.leaves.push(output.id()),
            _ => {}
        }
    }
}
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
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.tl_drawn = true;
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
    WlShm,
    WlShmPool,
    WlBuffer,
    WlOutput,
    XdgToplevel
);
