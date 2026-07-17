//! ROBUSTNESS DEMO 3 — `surface_storm` (rapid create+destroy churn neither leaks nor crashes).
//!
//! One client creates and immediately tears down 200 full surface trees (`wl_surface` +
//! `xdg_surface` + `xdg_toplevel`) in a tight loop, destroying each in the protocol-correct order
//! (toplevel → xdg_surface → wl_surface) before minting the next. This hammers the adapter's
//! `new_surface`/`new_toplevel`/`teardown_surface` bookkeeping (the surface-id maps, callback/repaint
//! tables, output-membership set). The compositor must reclaim each surface without leaking scene state
//! or panicking.
//!
//! After the storm, a fresh well-behaved [`Neighbor`] composites an EXACT frame — proof the adapter is
//! still healthy. As a leak sentinel, the client also runs a SECOND storm of the same size AFTER the
//! neighbor is up and asserts the neighbor keeps compositing (its surface id was not trampled by the
//! churn, and the id space did not wedge).

mod common;
use common::*;

use std::time::Duration;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const STORM: usize = 200;
const NW: i32 = 130;
const NH: i32 = 95;
const NEIGHBOR: [u8; 4] = [0xF0, 0xC0, 0x20, 0xFF]; // amber survivor

struct Storm;

#[test]
fn surface_storm() {
    let h = Harness::start("surface_storm");

    let conn = Connection::connect_to_env().expect("storm connect");
    let (globals, mut queue) = registry_queue_init::<Storm>(&conn).expect("storm registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // ---- storm 1: 200 surface trees created and torn down in a tight loop ----
    run_storm(&compositor, &wm_base, &qh, &mut queue, STORM);

    // Let the serve loop drain the last teardowns.
    std::thread::sleep(Duration::from_millis(50));

    // ---- survivor: a normal client composites an exact frame after the storm ----
    let mut neighbor = Neighbor::map(&h.runtime_dir, "survivor", NW, NH, NEIGHBOR);
    let frame = neighbor.assert_presents(&h.captures);
    save_frame("surface_storm-survivor", &frame);

    // ---- storm 2: churn again WHILE the neighbor is mapped; it must keep compositing ----
    run_storm(&compositor, &wm_base, &qh, &mut queue, STORM);
    neighbor.pump();
    let after = pump_until(
        &mut neighbor.queue,
        &mut neighbor.app,
        &h.captures,
        5,
        |f| f.width == NW && f.pixel_is(1, 1, NEIGHBOR),
    )
    .expect("neighbor stopped compositing after a second storm (leak/corruption?)");
    assert_eq!(
        after.pixel(NW / 2, NH / 2).unwrap(),
        NEIGHBOR,
        "neighbor still solid amber after both storms"
    );

    h.shutdown();
}

/// Create and immediately destroy `n` full surface trees, roundtripping periodically so the server keeps
/// pace and the loop cannot outrun its own destroy requests.
fn run_storm(
    compositor: &WlCompositor,
    wm_base: &XdgWmBase,
    qh: &QueueHandle<Storm>,
    queue: &mut EventQueue<Storm>,
    n: usize,
) {
    for i in 0..n {
        let surface: WlSurface = compositor.create_surface(qh, ());
        let xdg: XdgSurface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel: XdgToplevel = xdg.get_toplevel(qh, ());
        surface.commit(); // ask for the initial configure, then immediately tear the tree down
                          // Protocol-correct teardown order: toplevel → xdg_surface → wl_surface.
        toplevel.destroy();
        xdg.destroy();
        surface.destroy();
        if i % 25 == 0 {
            let _ = queue.roundtrip(&mut Storm);
        }
    }
    let _ = queue.roundtrip(&mut Storm);
}

// ---------- dispatch plumbing ----------
impl Dispatch<WlRegistry, GlobalListContents> for Storm {
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
impl Dispatch<XdgWmBase, ()> for Storm {
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
impl Dispatch<XdgSurface, ()> for Storm {
    fn event(
        _: &mut Self,
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
        }
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Storm {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlSurface, XdgToplevel);
