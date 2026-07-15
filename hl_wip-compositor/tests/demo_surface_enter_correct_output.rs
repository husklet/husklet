//! DEMO — `surface_enter_correct_output`.
//!
//! On a multi-monitor layout, `wl_surface.enter` must name the SPECIFIC `wl_output` a surface is displayed
//! on — the signal a toolkit uses to pick the right per-monitor render scale. This demo stands the
//! compositor up on two side-by-side outputs (A `1920×1080@0,0`, B `2560×1440@1920,0`), binds BOTH
//! `wl_output`s (identifying which proxy is A and which is B by their advertised geometry position), maps a
//! toplevel, and asserts:
//!
//!   * on map the surface enters output A (the primary — new surfaces start there);
//!   * routing it to a point inside output B's region flips membership: a `leave` for A, then an `enter`
//!     naming EXACTLY output B;
//!   * routing it back to a point inside A flips it again: a `leave` for B, then a second `enter` for A.
//!
//! The route is driven through the host/window-manager input seam (`InputCommand::MoveToplevelToPoint`),
//! which resolves the output whose logical rectangle contains the point — genuine position-based routing.
//! No PNG: the evidence is the exact `wl_output` each enter/leave names.

mod common;
use common::*;

use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;

use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_output::{self, WlOutput}, wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::{self, WlSurface},
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 120;
const H: i32 = 90;
const COL: [u8; 4] = [0x40, 0xC0, 0x60, 0xFF];

/// A bound `wl_output` and the geometry-x that identifies it (A at x=0, B at x=1920).
struct BoundOutput {
    id: ObjectId,
    geometry_x: Option<i32>,
}

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    outputs: Vec<BoundOutput>,
    enters: Vec<ObjectId>,
    leaves: Vec<ObjectId>,
}

impl App {
    /// The bound-output object id whose advertised geometry-x equals `x`.
    fn id_at_x(&self, x: i32) -> ObjectId {
        self.outputs
            .iter()
            .find(|o| o.geometry_x == Some(x))
            .unwrap_or_else(|| panic!("no bound wl_output at geometry x={x}"))
            .id
            .clone()
    }
    fn outputs_ready(&self) -> bool {
        self.outputs.len() == 2 && self.outputs.iter().all(|o| o.geometry_x.is_some())
    }
}

#[test]
fn surface_enter_correct_output() {
    std::env::set_var("HL_OUTPUTS", "1920x1080@0,0;2560x1440@1920,0*2");
    let h = Harness::start("surface_enter_correct_output");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let buffer = make_buffer(&shm, &qh, &h.runtime_dir, "seco", W, H, &solid(W, H, COL));
    let surface = compositor.create_surface(&qh, ());

    let mut app = App {
        surface: surface.clone(),
        buffer: buffer.clone(),
        drawn: false,
        frame_done: false,
        outputs: Vec::new(),
        enters: Vec::new(),
        leaves: Vec::new(),
    };

    // Bind BOTH advertised wl_outputs BEFORE mapping, so enter/leave target our proxies. Each carries its
    // index into `app.outputs`; its geometry event fills in the identifying x.
    let registry = globals.registry();
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == WlOutput::interface().name {
                let idx = app.outputs.len();
                let output: WlOutput = registry.bind(global.name, global.version.min(4).max(2), &qh, idx);
                app.outputs.push(BoundOutput { id: output.id(), geometry_x: None });
                std::mem::forget(output);
            }
        }
    });
    assert_eq!(app.outputs.len(), 2, "two wl_outputs advertised");

    // Learn each output's geometry position so we can name A vs B in the assertions.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.outputs_ready() {
        assert!(Instant::now() < deadline, "outputs never reported geometry");
        queue.blocking_dispatch(&mut app).expect("dispatch output geometry");
    }
    let a_id = app.id_at_x(0);
    let b_id = app.id_at_x(1920);

    // Map the toplevel.
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-surface-enter-correct-output".into());
    surface.commit();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }

    // ---- map: exactly one enter, naming output A (the primary) ----
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.enters.is_empty() {
        assert!(Instant::now() < deadline, "no enter after mapping");
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.enters, vec![a_id.clone()], "map enters output A (primary)");
    assert!(app.leaves.is_empty(), "no leave yet");

    // ---- route to a point inside B: leave A, enter B ----
    h.input_tx
        .send(InputCommand::MoveToplevelToPoint { index: 0, x: 2020, y: 100 })
        .expect("route to B");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.enters.len() < 2 {
        assert!(Instant::now() < deadline, "no enter for B after routing (enters={:?})", app.enters);
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.leaves, vec![a_id.clone()], "routing to B leaves A exactly once");
    assert_eq!(app.enters, vec![a_id.clone(), b_id.clone()], "second enter names EXACTLY output B");

    // ---- route back to a point inside A: leave B, enter A again ----
    h.input_tx
        .send(InputCommand::MoveToplevelToPoint { index: 0, x: 100, y: 100 })
        .expect("route to A");
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.enters.len() < 3 {
        assert!(Instant::now() < deadline, "no re-enter for A after routing back (enters={:?})", app.enters);
        let _ = queue.roundtrip(&mut app);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.leaves, vec![a_id.clone(), b_id.clone()], "routing back leaves B exactly once");
    assert_eq!(
        app.enters,
        vec![a_id.clone(), b_id.clone(), a_id.clone()],
        "third enter names output A again",
    );

    h.shutdown();
    let _ = toplevel;
    let _ = xdg;
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlOutput, usize> for App {
    fn event(app: &mut Self, _: &WlOutput, e: <WlOutput as Proxy>::Event, idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Geometry { x, .. } = e {
            app.outputs[*idx].geometry_x = Some(x);
        }
    }
}
impl Dispatch<WlSurface, ()> for App {
    fn event(app: &mut Self, _: &WlSurface, e: <WlSurface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_surface::Event::Enter { output } => app.enters.push(output.id()),
            wl_surface::Event::Leave { output } => app.leaves.push(output.id()),
            _ => {}
        }
    }
}
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
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.frame_done = true;
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
ignore!(WlCompositor, WlShm, WlShmPool, WlBuffer, XdgToplevel);
