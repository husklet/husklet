//! DEMO (batch-3) — `many_surface_zorder` (K=6 stacked subsurfaces composite in EXACT paint order).
//!
//! A toplevel with SIX overlapping desynchronized `wl_subsurface`s, all covering one client_harness region, each
//! a distinct color. The headless presenter captures LAYERS (not a blended framebuffer), so the z-order
//! evidence is the PRESENT ORDER within a single compose cycle: `compose_frame` emits the tree bottom →
//! top, so the six layers present with SIX CONTIGUOUS serials in stacking order — exactly the order a
//! real backend blends. The test asserts:
//!
//!   * default stacking (creation order) composites the six as `[0,1,2,3,4,5]` bottom → top — the six
//!     subsurface present-serials are contiguous and ascending in that order;
//!   * after `sub0.place_above(sub5)` the order becomes `[1,2,3,4,5,0]` (surface 0 raised to the top);
//!   * at every point in the client_harness-overlap region the reconstructed composite equals the TOPMOST
//!     surface's color — before the reorder that is surface 5, after it is surface 0.
//!
//! A composited PNG is written for each stacking so the overlap visibly flips color.

mod client_harness;
use client_harness::*;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::CapturedFrame;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const K: usize = 6;
const TL_W: i32 = 160;
const TL_H: i32 = 130;
const TL: [u8; 4] = [0x30, 0x30, 0x38, 0xFF]; // dark slate

const SUB_W: i32 = 60;
const SUB_H: i32 = 45;
// Cascaded positions; all six overlap the client_harness region x[60,90) y[50,70).
fn sub_pos(i: usize) -> (i32, i32) {
    (30 + i as i32 * 6, 25 + i as i32 * 5)
}
const COLORS: [[u8; 4]; K] = [
    [0xE0, 0x20, 0x20, 0xFF], // 0 red
    [0x20, 0xE0, 0x20, 0xFF], // 1 green
    [0x20, 0x20, 0xE0, 0xFF], // 2 blue
    [0xE0, 0xE0, 0x20, 0xFF], // 3 yellow
    [0xE0, 0x20, 0xE0, 0xFF], // 4 magenta
    [0x20, 0xE0, 0xE0, 0xFF], // 5 cyan
];
// Points inside the client_harness overlap of all six subsurfaces (toplevel space).
const OVERLAP_POINTS: [(i32, i32); 3] = [(75, 60), (65, 55), (85, 68)];

/// A capture is subsurface `i` iff it has the subsurface geometry and its solid color.
fn is_sub(f: &CapturedFrame, i: usize) -> bool {
    f.width == SUB_W && f.height == SUB_H && f.pixel_is(SUB_W / 2, SUB_H / 2, COLORS[i])
}

/// Newest present serial captured for subsurface `i`, if any.
fn newest_serial(caps: &Arc<Mutex<Vec<CapturedFrame>>>, i: usize) -> Option<u64> {
    caps.lock()
        .unwrap()
        .iter()
        .filter(|f| is_sub(f, i))
        .map(|f| f.serial)
        .max()
}

/// The current highest serial across ALL captures — a baseline a fresh compose cycle must exceed.
fn max_serial(caps: &Arc<Mutex<Vec<CapturedFrame>>>) -> u64 {
    caps.lock()
        .unwrap()
        .iter()
        .map(|f| f.serial)
        .max()
        .unwrap_or(0)
}

struct App {
    tl_surface: WlSurface,
    tl_buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
}

/// After a full re-present, return the six subsurface present-serials paired with their index, and assert
/// they form ONE contiguous compose cycle (max-min == K-1) — proof all six composited together.
fn read_stacking(
    queue: &mut wayland_client::EventQueue<App>,
    app: &mut App,
    caps: &Arc<Mutex<Vec<CapturedFrame>>>,
    baseline: u64,
) -> Vec<(usize, u64)> {
    // Poll until every subsurface has re-presented after the baseline (its whole tree composed afresh).
    let ok = pump_while(queue, app, 5, |_| {
        (0..K).all(|i| {
            newest_serial(caps, i)
                .map(|s| s > baseline)
                .unwrap_or(false)
        })
    });
    assert!(ok, "not all {K} subsurfaces re-presented after the repaint");
    let mut serials: Vec<(usize, u64)> = (0..K)
        .map(|i| (i, newest_serial(caps, i).unwrap()))
        .collect();
    let smin = serials.iter().map(|(_, s)| *s).min().unwrap();
    let smax = serials.iter().map(|(_, s)| *s).max().unwrap();
    assert_eq!(
        smax - smin,
        (K as u64) - 1,
        "the {K} subsurfaces present in ONE contiguous compose cycle"
    );
    serials.sort_by_key(|(_, s)| *s);
    serials
}

#[test]
fn many_surface_zorder() {
    let h = Harness::start("many_surface_zorder");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let subcompositor: WlSubcompositor = globals.bind(&qh, 1..=1, ()).expect("wl_subcompositor");

    let tl_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "tl",
        TL_W,
        TL_H,
        &solid(TL_W, TL_H, TL),
    );
    let sub_buffers: Vec<WlBuffer> = (0..K)
        .map(|i| {
            make_buffer(
                &shm,
                &qh,
                &h.runtime_dir,
                &format!("s{i}"),
                SUB_W,
                SUB_H,
                &solid(SUB_W, SUB_H, COLORS[i]),
            )
        })
        .collect();

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, ());
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-zorder".into());
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
        pump_until(&mut queue, &mut app, &h.captures, 5, |f| f.width == TL_W
            && f.pixel_is(1, 1, TL))
        .is_some(),
        "toplevel never composited",
    );

    // ---- create K overlapping desync subsurfaces in ascending order (0 first) ----
    let mut subs: Vec<WlSurface> = Vec::new();
    let mut sub_roles: Vec<WlSubsurface> = Vec::new();
    for i in 0..K {
        let s = compositor.create_surface(&qh, ());
        let role: WlSubsurface = subcompositor.get_subsurface(&s, &tl_surface, &qh, ());
        let (px, py) = sub_pos(i);
        role.set_position(px, py);
        role.set_desync();
        s.attach(Some(&sub_buffers[i]), 0, 0);
        s.damage(0, 0, SUB_W, SUB_H);
        s.commit();
        subs.push(s);
        sub_roles.push(role);
    }
    tl_surface.commit();

    // ---- default stacking: creation order [0,1,2,3,4,5] bottom -> top ----
    let baseline = max_serial(&h.captures);
    // Force a full re-present of the whole tree.
    tl_surface.attach(Some(&tl_buffer), 0, 0);
    tl_surface.damage(0, 0, TL_W, TL_H);
    tl_surface.commit();
    let order0 = read_stacking(&mut queue, &mut app, &h.captures, baseline);
    let order0_idx: Vec<usize> = order0.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        order0_idx,
        vec![0, 1, 2, 3, 4, 5],
        "default stacking is creation order, bottom -> top"
    );

    // The topmost (last presented) is surface 5 (cyan): the composite over the overlap must equal COLORS[5].
    confront_overlap(&h.captures, &subs, "many_surface_zorder-0_default_top5", 5);

    // ---- reorder: raise surface 0 to the very top ----
    sub_roles[0].place_above(&subs[K - 1]);
    let baseline = max_serial(&h.captures);
    tl_surface.attach(Some(&tl_buffer), 0, 0);
    tl_surface.damage(0, 0, TL_W, TL_H);
    tl_surface.commit();
    let order1 = read_stacking(&mut queue, &mut app, &h.captures, baseline);
    let order1_idx: Vec<usize> = order1.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        order1_idx,
        vec![1, 2, 3, 4, 5, 0],
        "after place_above, surface 0 is on top"
    );

    // Topmost is now surface 0 (red): the composite over the overlap must equal COLORS[0].
    confront_overlap(&h.captures, &subs, "many_surface_zorder-1_raised_top0", 0);

    h.shutdown();
}

/// Reconstruct the composited stack (bottom -> top by newest present serial) into a PNG, and assert that
/// at every overlap point the composited pixel equals the topmost surface `top_idx`'s color — the exact
/// paint-order result a viewer sees.
fn confront_overlap(
    caps: &Arc<Mutex<Vec<CapturedFrame>>>,
    subs: &[WlSurface],
    name: &str,
    top_idx: usize,
) {
    // Order the six layers by their newest serial (== bottom -> top stacking this cycle).
    let mut layers: Vec<(usize, CapturedFrame)> = Vec::new();
    for i in 0..K {
        let f = caps
            .lock()
            .unwrap()
            .iter()
            .filter(|f| is_sub(f, i))
            .max_by_key(|f| f.serial)
            .cloned();
        if let Some(f) = f {
            layers.push((i, f));
        }
    }
    layers.sort_by_key(|(_, f)| f.serial);
    let _ = subs; // subs kept alive by the caller; positions come from sub_pos.

    let ordered: Vec<(&CapturedFrame, i32, i32)> = layers
        .iter()
        .map(|(i, f)| (f, sub_pos(*i).0, sub_pos(*i).1))
        .collect();
    save_composited(name, TL_W, TL_H, TL, &ordered);

    // Rebuild the same composite in-memory and assert the overlap equals the topmost color.
    let top_color = COLORS[top_idx];
    assert_eq!(
        layers.last().map(|(i, _)| *i),
        Some(top_idx),
        "topmost layer is surface {top_idx}"
    );
    for (px, py) in OVERLAP_POINTS {
        // Every subsurface covers this point, so the top-most in paint order owns the pixel.
        let mut seen = TL;
        for (i, _) in &layers {
            let (ox, oy) = sub_pos(*i);
            if px >= ox && px < ox + SUB_W && py >= oy && py < oy + SUB_H {
                seen = COLORS[*i];
            }
        }
        assert_eq!(
            seen, top_color,
            "overlap point ({px},{py}) shows the topmost surface {top_idx}"
        );
    }
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
    WlSubcompositor,
    WlSubsurface,
    XdgToplevel
);
