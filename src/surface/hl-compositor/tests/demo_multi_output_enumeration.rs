//! DEMO — `multi_output_enumeration`.
//!
//! A multi-monitor compositor advertises MORE THAN ONE `wl_output`, each with its own geometry
//! (position), mode (resolution + refresh), and scale — the fields GTK/Chrome/Qt read to place windows and
//! choose a render scale per monitor. This demo stands the compositor up on a two-output layout (via
//! `$HL_OUTPUTS`), then drives a real in-process wayland-client that ENUMERATES every advertised
//! `wl_output` global, binds each, pairs it with its `zxdg_output_v1`, and asserts EXACTLY the fields each
//! monitor reports:
//!
//!   * output A — `1920×1080@0,0` scale 1: `wl_output.geometry` x/y = `(0, 0)` + make/model, transform
//!     Normal, `wl_output.mode` `1920×1080@60000` mHz, `wl_output.scale` 1, and xdg-output
//!     `logical_position (0, 0)` / `logical_size 1920×1080`;
//!   * output B — `2560×1440@1920,0` scale 2: `wl_output.geometry` x/y = `(1920, 0)`, `wl_output.mode`
//!     `2560×1440@60000`, `wl_output.scale` 2, and xdg-output `logical_position (1920, 0)` / `logical_size
//!     1280×720` (mode ÷ scale 2).
//!
//! No PNG: this asserts advertised OUTPUT geometry across two monitors, not composited pixels, so its
//! evidence is the exact wire values a client receives for each.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_output::{self, Transform, WlOutput},
    wl_registry::WlRegistry,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};

/// Everything a client learns about one advertised `wl_output` + its `zxdg_output_v1`.
#[derive(Default, Clone)]
struct OutputInfo {
    /// `wl_output.geometry`: `(x, y, transform, make, model)`.
    geometry: Option<(i32, i32, Transform, String, String)>,
    /// `wl_output.mode`: `(width, height, refresh_mHz)`.
    mode: Option<(i32, i32, i32)>,
    /// `wl_output.scale`.
    scale: Option<i32>,
    /// xdg-output `logical_position`.
    logical_position: Option<(i32, i32)>,
    /// xdg-output `logical_size`.
    logical_size: Option<(i32, i32)>,
}

impl OutputInfo {
    fn complete(&self) -> bool {
        self.geometry.is_some()
            && self.mode.is_some()
            && self.scale.is_some()
            && self.logical_position.is_some()
            && self.logical_size.is_some()
    }
}

struct App {
    outputs: Vec<OutputInfo>,
}

#[test]
fn multi_output_enumeration() {
    // Two monitors side by side: a scale-1 1080p at the origin and a scale-2 1440p to its right. Set before
    // the harness spawns the serve thread (read in `HlState::new`).
    std::env::set_var("HL_OUTPUTS", "1920x1080@0,0;2560x1440@1920,0*2");
    let h = Harness::start("multi_output_enumeration");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let xdg_output_mgr: ZxdgOutputManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .expect("zxdg_output_manager_v1 (adapter must advertise it)");

    // Enumerate EVERY advertised `wl_output` global — a multi-monitor compositor advertises one per
    // monitor — and bind each, pairing it with its xdg-output. Each gets an index into `app.outputs`.
    let registry = globals.registry();
    let mut count = 0usize;
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == WlOutput::interface().name {
                let version = global.version.min(4).max(2);
                let output: WlOutput = registry.bind(global.name, version, &qh, count);
                let xdg: ZxdgOutputV1 = xdg_output_mgr.get_xdg_output(&output, &qh, count);
                // Keep both proxies alive for the client's lifetime so their events keep dispatching.
                std::mem::forget(output);
                std::mem::forget(xdg);
                count += 1;
            }
        }
    });
    assert_eq!(count, 2, "the compositor advertises exactly two wl_outputs");

    let mut app = App {
        outputs: vec![OutputInfo::default(); count],
    };

    // Pump until every output has reported all five field groups.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.outputs.iter().all(OutputInfo::complete) {
        assert!(
            Instant::now() < deadline,
            "output enumeration incomplete: {:?}",
            summarize(&app)
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch output enumeration");
    }

    // Identify the monitors by their distinct mode WIDTH (order of global enumeration is not guaranteed).
    let a = app
        .outputs
        .iter()
        .find(|o| o.mode.unwrap().0 == 1920)
        .expect("output A (1920 wide) advertised");
    let b = app
        .outputs
        .iter()
        .find(|o| o.mode.unwrap().0 == 2560)
        .expect("output B (2560 wide) advertised");

    // ---- Output A: 1920×1080@0,0, scale 1 ----
    let (ax, ay, at, ref amake, ref amodel) = a.geometry.clone().unwrap();
    assert_eq!((ax, ay), (0, 0), "A geometry position is the origin");
    assert_eq!(at, Transform::Normal, "A is not rotated");
    assert_eq!(
        (amake.as_str(), amodel.as_str()),
        ("hl", "hl-virtual"),
        "A make/model"
    );
    assert_eq!(
        a.mode.unwrap(),
        (1920, 1080, 60_000),
        "A mode is 1920×1080@60000 mHz"
    );
    assert_eq!(a.scale.unwrap(), 1, "A scale is 1");
    assert_eq!(
        a.logical_position.unwrap(),
        (0, 0),
        "A xdg logical position is the origin"
    );
    assert_eq!(
        a.logical_size.unwrap(),
        (1920, 1080),
        "A xdg logical size == mode ÷ scale 1"
    );

    // ---- Output B: 2560×1440@1920,0, scale 2 ----
    let (bx, by, bt, ref bmake, ref bmodel) = b.geometry.clone().unwrap();
    assert_eq!(
        (bx, by),
        (1920, 0),
        "B geometry position is to the right of A (x=1920)"
    );
    assert_eq!(bt, Transform::Normal, "B is not rotated");
    assert_eq!(
        (bmake.as_str(), bmodel.as_str()),
        ("hl", "hl-virtual"),
        "B make/model"
    );
    assert_eq!(
        b.mode.unwrap(),
        (2560, 1440, 60_000),
        "B mode is 2560×1440@60000 mHz (physical, unscaled)"
    );
    assert_eq!(b.scale.unwrap(), 2, "B scale is 2");
    assert_eq!(
        b.logical_position.unwrap(),
        (1920, 0),
        "B xdg logical position is (1920, 0)"
    );
    assert_eq!(
        b.logical_size.unwrap(),
        (1280, 720),
        "B xdg logical size == mode ÷ scale 2 == 1280×720"
    );

    h.shutdown();
}

fn summarize(app: &App) -> Vec<(Option<(i32, i32, i32)>, Option<i32>)> {
    app.outputs.iter().map(|o| (o.mode, o.scale)).collect()
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlOutput, usize> for App {
    fn event(
        app: &mut Self,
        _: &WlOutput,
        e: <WlOutput as Proxy>::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let info = &mut app.outputs[*idx];
        match e {
            wl_output::Event::Geometry {
                x,
                y,
                make,
                model,
                transform,
                ..
            } => {
                info.geometry = Some((
                    x,
                    y,
                    transform.into_result().unwrap_or(Transform::Normal),
                    make,
                    model,
                ));
            }
            wl_output::Event::Mode {
                width,
                height,
                refresh,
                ..
            } => {
                info.mode = Some((width, height, refresh));
            }
            wl_output::Event::Scale { factor } => {
                info.scale = Some(factor);
            }
            _ => {}
        }
    }
}
impl Dispatch<ZxdgOutputV1, usize> for App {
    fn event(
        app: &mut Self,
        _: &ZxdgOutputV1,
        e: <ZxdgOutputV1 as Proxy>::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let info = &mut app.outputs[*idx];
        match e {
            zxdg_output_v1::Event::LogicalPosition { x, y } => info.logical_position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                info.logical_size = Some((width, height))
            }
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
impl Dispatch<ZxdgOutputManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZxdgOutputManagerV1,
        _: <ZxdgOutputManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
