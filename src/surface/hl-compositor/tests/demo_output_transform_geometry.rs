//! DEMO — `output_transform_geometry`.
//!
//! A `wl_output` that is physically rotated advertises a `wl_output.transform`, and — crucially — a
//! 90°/270° transform SWAPS the logical width/height the compositor lays clients out in (a 1920×1080
//! panel rotated 90° presents a 1080×1920 logical area). Clients read this from `wl_output.geometry`
//! (the transform field) and from xdg-output's `logical_size`, and toolkits size/place windows against
//! it.
//!
//! This demo stands the compositor up on a 90°-rotated output (via `$HL_OUTPUT_TRANSFORM`), then drives a
//! real in-process wayland-client that binds `wl_output` + `zxdg_output_manager_v1` and asserts EXACTLY:
//!
//!   * `wl_output.geometry.transform == 90` (the rotation is carried by the transform, not the mode);
//!   * `wl_output.mode` still reports the PHYSICAL panel size 1920×1080 (unswapped);
//!   * xdg-output `logical_size == 1080×1920` (SWAPPED for the 90° rotation, at scale 1).
//!
//! No PNG: this demo asserts advertised OUTPUT GEOMETRY, not composited pixels, so its evidence is the
//! exact wire values a client receives.

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

struct App {
    geometry_transform: Option<Transform>,
    mode_size: Option<(i32, i32)>,
    logical_size: Option<(i32, i32)>,
}

#[test]
fn output_transform_geometry() {
    // Stand the compositor up on a 90°-rotated output. The env var is read in `HlState::new` (each demo
    // owns its whole process), so it must be set BEFORE the harness spawns the serve thread.
    std::env::set_var("HL_OUTPUT_TRANSFORM", "90");
    let h = Harness::start("output_transform_geometry");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    // wl_output v2+ delivers geometry (with the transform) and mode; the xdg-output manager delivers the
    // logical size. Both must be advertised for a modern toolkit to learn a rotated output's geometry.
    let output: WlOutput = globals
        .bind(&qh, 2..=4, ())
        .expect("wl_output (>=v2 for transform)");
    let xdg_output_mgr: ZxdgOutputManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .expect("zxdg_output_manager_v1 (adapter must advertise it)");
    let _xdg_output: ZxdgOutputV1 = xdg_output_mgr.get_xdg_output(&output, &qh, ());

    let mut app = App {
        geometry_transform: None,
        mode_size: None,
        logical_size: None,
    };

    // Pump until all three pieces of geometry have arrived.
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.geometry_transform.is_none() || app.mode_size.is_none() || app.logical_size.is_none()
    {
        assert!(
            Instant::now() < deadline,
            "output geometry incomplete: transform={:?} mode={:?} logical={:?}",
            app.geometry_transform,
            app.mode_size,
            app.logical_size
        );
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch output geometry");
    }

    // The rotation is carried by the transform field...
    assert_eq!(
        app.geometry_transform,
        Some(Transform::_90),
        "wl_output.geometry advertises the 90° transform"
    );
    // ...while the mode still reports the PHYSICAL panel size, unswapped.
    assert_eq!(
        app.mode_size,
        Some((1920, 1080)),
        "wl_output.mode reports the physical panel size (unswapped)"
    );
    // xdg-output's logical size is SWAPPED for the 90° rotation (scale 1).
    assert_eq!(
        app.logical_size,
        Some((1080, 1920)),
        "xdg-output logical size is swapped for the 90° rotation"
    );

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlOutput, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlOutput,
        e: <WlOutput as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_output::Event::Geometry { transform, .. } => {
                // `transform` is a WEnum; capture the resolved value.
                app.geometry_transform = transform.into_result().ok();
            }
            wl_output::Event::Mode { width, height, .. } => {
                app.mode_size = Some((width, height));
            }
            _ => {}
        }
    }
}
impl Dispatch<ZxdgOutputV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZxdgOutputV1,
        e: <ZxdgOutputV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::LogicalSize { width, height } = e {
            app.logical_size = Some((width, height));
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
