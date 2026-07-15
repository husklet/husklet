//! DEMO 1 — `popup_composites_at_positioner_rect`.
//!
//! A real `wayland-client` maps a toplevel (color A), then opens an `xdg_popup` via an `xdg_positioner`
//! (anchor/gravity/offset) in a distinct color B, then a NESTED popup (submenu) anchored on the first
//! popup in color C. We assert — with EXACT pixels — that each popup composited at exactly the
//! positioner-resolved rectangle (not merely "a popup exists"), in its own color, and that the composite
//! order is toplevel -> popup -> submenu. A viewable composited PNG is written for human confirmation.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::XdgPopup,
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

// Toplevel (color A) — blue.
const TL_W: i32 = 300;
const TL_H: i32 = 200;
const TL: [u8; 4] = [0x20, 0x20, 0xC0, 0xFF];

// Popup (color B) — red. Positioner: anchor rect (100,100,20,20), anchor BottomLeft -> (100,120),
// gravity BottomRight -> grows down-right, offset (7,9) => geometry origin (107,129) in the toplevel.
const POP_W: i32 = 40;
const POP_H: i32 = 30;
const POP: [u8; 4] = [0xE0, 0x10, 0x10, 0xFF];
const ANCHOR1: (i32, i32, i32, i32) = (100, 100, 20, 20);
const OFFSET1: (i32, i32) = (7, 9);
const EXPECT_POP: (i32, i32) = (107, 129);

// Nested popup / submenu (color C) — yellow. Anchored on the FIRST popup's window geometry:
// anchor rect (30,20,4,4), anchor BottomRight -> (34,24), gravity BottomRight, offset (2,3) => geometry
// origin (36,27) relative to popup1 => (107+36, 129+27) = (143,156) relative to the toplevel.
const SUB_W: i32 = 24;
const SUB_H: i32 = 18;
const SUBM: [u8; 4] = [0xE0, 0xD0, 0x10, 0xFF];
const ANCHOR2: (i32, i32, i32, i32) = (30, 20, 4, 4);
const OFFSET2: (i32, i32) = (2, 3);
const EXPECT_SUB: (i32, i32) = (143, 156);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Toplevel,
    Popup1,
    Popup2,
}

struct App {
    tl_surface: WlSurface,
    tl_buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
    pop1_surface: WlSurface,
    pop1_buffer: WlBuffer,
    pop1_drawn: bool,
    pop2_surface: Option<WlSurface>,
    pop2_buffer: WlBuffer,
    pop2_drawn: bool,
}

#[test]
fn popup_composites_at_positioner_rect() {
    let h = Harness::start("popup_positioner");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let tl_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "tl", TL_W, TL_H, &solid(TL_W, TL_H, TL));
    let pop1_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "p1", POP_W, POP_H, &solid(POP_W, POP_H, POP));
    let pop2_buffer = make_buffer(&shm, &qh, &h.runtime_dir, "p2", SUB_W, SUB_H, &solid(SUB_W, SUB_H, SUBM));

    // ---- map the toplevel ----
    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, Role::Toplevel);
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-popup".into());
    tl_surface.commit();

    let pop1_surface = compositor.create_surface(&qh, ());

    let mut app = App {
        tl_surface: tl_surface.clone(),
        tl_buffer,
        tl_drawn: false,
        tl_frame_done: false,
        pop1_surface: pop1_surface.clone(),
        pop1_buffer,
        pop2_surface: None,
        pop2_buffer,
        pop2_drawn: false,
        pop1_drawn: false,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let tl_frame = wait_for(&h.captures, 5, |f| f.width == TL_W && f.pixel_is(TL_W / 2, TL_H / 2, TL))
        .expect("toplevel never composited its base color");

    // ---- popup 1 via a positioner + grab ----
    let pos1: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos1.set_size(POP_W, POP_H);
    pos1.set_anchor_rect(ANCHOR1.0, ANCHOR1.1, ANCHOR1.2, ANCHOR1.3);
    pos1.set_anchor(xdg_positioner::Anchor::BottomLeft);
    pos1.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos1.set_offset(OFFSET1.0, OFFSET1.1);
    let pop1_xdg = wm_base.get_xdg_surface(&pop1_surface, &qh, Role::Popup1);
    let popup1: XdgPopup = pop1_xdg.get_popup(Some(&tl_xdg), &pos1, &qh, ());
    popup1.grab(&seat, 0);
    pop1_surface.commit();

    let pop1_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == POP_W && f.height == POP_H && (f.x, f.y) == EXPECT_POP && f.pixel_is(POP_W / 2, POP_H / 2, POP)
    })
    .expect("popup1 never composited at its resolved positioner rect");
    assert_eq!((pop1_frame.x, pop1_frame.y), EXPECT_POP, "popup1 exact placement");
    assert_eq!(pop1_frame.pixel(POP_W / 2, POP_H / 2).unwrap(), POP, "popup1 color");
    // Popup B occupies EXACTLY the placed rect (all four sampled corners are B, one-past-edge is not
    // sampled here because the presenter captures the layer tightly at its own size).
    for &(sx, sy) in &[(0, 0), (POP_W - 1, 0), (0, POP_H - 1), (POP_W - 1, POP_H - 1)] {
        assert_eq!(pop1_frame.pixel(sx, sy).unwrap(), POP, "popup1 fully B at ({sx},{sy})");
    }

    // ---- nested popup (submenu) anchored on popup 1 ----
    let pop2_surface = compositor.create_surface(&qh, ());
    app.pop2_surface = Some(pop2_surface.clone());
    let pos2: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos2.set_size(SUB_W, SUB_H);
    pos2.set_anchor_rect(ANCHOR2.0, ANCHOR2.1, ANCHOR2.2, ANCHOR2.3);
    pos2.set_anchor(xdg_positioner::Anchor::BottomRight);
    pos2.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos2.set_offset(OFFSET2.0, OFFSET2.1);
    let pop2_xdg = wm_base.get_xdg_surface(&pop2_surface, &qh, Role::Popup2);
    let popup2: XdgPopup = pop2_xdg.get_popup(Some(&pop1_xdg), &pos2, &qh, ());
    popup2.grab(&seat, 0);
    pop2_surface.commit();

    let pop2_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == SUB_W && f.height == SUB_H && (f.x, f.y) == EXPECT_SUB && f.pixel_is(SUB_W / 2, SUB_H / 2, SUBM)
    })
    .expect("nested popup never composited at its resolved rect");
    assert_eq!((pop2_frame.x, pop2_frame.y), EXPECT_SUB, "submenu exact placement");
    assert_eq!(pop2_frame.pixel(SUB_W / 2, SUB_H / 2).unwrap(), SUBM, "submenu color");

    // Distinct colors across all three layers.
    assert_ne!(POP, TL);
    assert_ne!(SUBM, TL);
    assert_ne!(SUBM, POP);

    // Composite order: toplevel (bottom) -> popup1 -> submenu (top), by present serial.
    assert!(tl_frame.serial < pop1_frame.serial, "toplevel presents below popup1");
    assert!(pop1_frame.serial < pop2_frame.serial, "popup1 presents below the submenu");

    // Human-viewable composited PNG (blended layers at their resolved offsets).
    save_composited(
        "popup_positioner-frame0",
        TL_W,
        TL_H,
        TL,
        &[(&pop1_frame, EXPECT_POP.0, EXPECT_POP.1), (&pop2_frame, EXPECT_SUB.0, EXPECT_SUB.1)],
    );

    h.shutdown();
}

// ---------- wayland-client Dispatch plumbing ----------

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
impl Dispatch<XdgSurface, Role> for App {
    fn event(app: &mut Self, xdg: &XdgSurface, e: <XdgSurface as Proxy>::Event, role: &Role, _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            match role {
                Role::Toplevel if !app.tl_drawn => {
                    app.tl_surface.attach(Some(&app.tl_buffer), 0, 0);
                    app.tl_surface.damage(0, 0, TL_W, TL_H);
                    let _cb: WlCallback = app.tl_surface.frame(qh, ());
                    app.tl_surface.commit();
                    app.tl_drawn = true;
                }
                Role::Popup1 if !app.pop1_drawn => {
                    app.pop1_surface.attach(Some(&app.pop1_buffer), 0, 0);
                    app.pop1_surface.damage(0, 0, POP_W, POP_H);
                    app.pop1_surface.commit();
                    app.pop1_drawn = true;
                }
                Role::Popup2 if !app.pop2_drawn => {
                    if let Some(s) = &app.pop2_surface {
                        s.attach(Some(&app.pop2_buffer), 0, 0);
                        s.damage(0, 0, SUB_W, SUB_H);
                        s.commit();
                        app.pop2_drawn = true;
                    }
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<XdgPopup, ()> for App {
    fn event(_: &mut Self, _: &XdgPopup, _: <XdgPopup as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
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
ignore!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, XdgToplevel, XdgPositioner);
