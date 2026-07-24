//! DEMO — `popup_nested_chain_dismiss` (a menu → submenu grab chain dismisses together, innermost first).
//!
//! A toplevel opens a grabbed popup (a menu), and that popup opens a SECOND grabbed popup parented to it
//! (a submenu) — a two-deep `xdg_popup.grab` chain. A single pointer press OUTSIDE the whole chain must:
//!
//!   * deliver `xdg_popup.popup_done` to BOTH popups, INNERMOST FIRST (submenu before menu — the spec's
//!     dismissal order);
//!   * remove BOTH popups from the composited frame (neither the menu nor the submenu layer is composited
//!     again after the dismiss), leaving the toplevel alone on screen.
//!
//! Extends `demo_popup_dismiss` (single popup) to the nested case real menu systems rely on.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use hl_compositor::adapter::smithay::{CapturedFrame, InputCommand};

const TL_W: i32 = 320;
const TL_H: i32 = 220;
const TL: [u8; 4] = [0x18, 0x18, 0xB0, 0xFF]; // blue

const P1_W: i32 = 60;
const P1_H: i32 = 40;
const P1: [u8; 4] = [0xE0, 0x20, 0x20, 0xFF]; // red menu
                                              // popup1 anchored inside the toplevel.
const P1_ANCHOR: (i32, i32, i32, i32) = (80, 60, 10, 10);
const P1_OFF: (i32, i32) = (5, 5);

const P2_W: i32 = 50;
const P2_H: i32 = 34;
const P2: [u8; 4] = [0x20, 0xE0, 0x20, 0xFF]; // green submenu
                                              // popup2 anchored inside popup1 (its coordinate space).
const P2_ANCHOR: (i32, i32, i32, i32) = (40, 10, 10, 10);
const P2_OFF: (i32, i32) = (4, 3);

const BTN_LEFT: u32 = 0x110;

#[derive(Clone, Copy, PartialEq)]
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

    p1_surface: WlSurface,
    p1_buffer: WlBuffer,
    p1_xdg: Option<XdgSurface>,
    p1_popup: Option<XdgPopup>,
    p1_drawn: bool,

    p2_surface: WlSurface,
    p2_buffer: WlBuffer,
    p2_xdg: Option<XdgSurface>,
    p2_popup: Option<XdgPopup>,
    p2_drawn: bool,

    /// popup_done arrival order, by popup id (1 = menu, 2 = submenu).
    dismiss_order: Vec<u8>,
}

fn is_p1(f: &CapturedFrame) -> bool {
    f.width == P1_W && f.height == P1_H && f.pixel_is(P1_W / 2, P1_H / 2, P1)
}
fn is_p2(f: &CapturedFrame) -> bool {
    f.width == P2_W && f.height == P2_H && f.pixel_is(P2_W / 2, P2_H / 2, P2)
}
fn is_tl(f: &CapturedFrame) -> bool {
    f.width == TL_W && f.height == TL_H && f.pixel_is(TL_W / 2, TL_H / 2, TL)
}

#[test]
fn popup_nested_chain_dismiss() {
    let h = Harness::start("popup_nested_chain");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");

    let tl_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "tl",
        TL_W,
        TL_H,
        &solid(TL_W, TL_H, TL),
    );
    let p1_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "p1",
        P1_W,
        P1_H,
        &solid(P1_W, P1_H, P1),
    );
    let p2_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "p2",
        P2_W,
        P2_H,
        &solid(P2_W, P2_H, P2),
    );

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, Role::Toplevel);
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-nested".into());
    tl_surface.commit();
    let p1_surface = compositor.create_surface(&qh, ());
    let p2_surface = compositor.create_surface(&qh, ());

    let mut app = App {
        tl_surface: tl_surface.clone(),
        tl_buffer,
        tl_drawn: false,
        tl_frame_done: false,
        p1_surface: p1_surface.clone(),
        p1_buffer,
        p1_xdg: None,
        p1_popup: None,
        p1_drawn: false,
        p2_surface: p2_surface.clone(),
        p2_buffer,
        p2_xdg: None,
        p2_popup: None,
        p2_drawn: false,
        dismiss_order: Vec::new(),
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, is_tl).is_some(),
        "toplevel never composited"
    );

    // ---- open popup1 (menu) grabbed on the toplevel ----
    let pos1: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos1.set_size(P1_W, P1_H);
    pos1.set_anchor_rect(P1_ANCHOR.0, P1_ANCHOR.1, P1_ANCHOR.2, P1_ANCHOR.3);
    pos1.set_anchor(xdg_positioner::Anchor::BottomLeft);
    pos1.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos1.set_offset(P1_OFF.0, P1_OFF.1);
    let p1_xdg = wm_base.get_xdg_surface(&p1_surface, &qh, Role::Popup1);
    let p1_popup: XdgPopup = p1_xdg.get_popup(Some(&tl_xdg), &pos1, &qh, 1u8);
    p1_popup.grab(&seat, 0);
    app.p1_xdg = Some(p1_xdg.clone());
    app.p1_popup = Some(p1_popup);
    p1_surface.commit();
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, is_p1).is_some(),
        "menu popup never composited"
    );

    // ---- open popup2 (submenu) grabbed on popup1 ----
    let pos2: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos2.set_size(P2_W, P2_H);
    pos2.set_anchor_rect(P2_ANCHOR.0, P2_ANCHOR.1, P2_ANCHOR.2, P2_ANCHOR.3);
    pos2.set_anchor(xdg_positioner::Anchor::BottomLeft);
    pos2.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos2.set_offset(P2_OFF.0, P2_OFF.1);
    let p2_xdg = wm_base.get_xdg_surface(&p2_surface, &qh, Role::Popup2);
    let p2_popup: XdgPopup = p2_xdg.get_popup(Some(&p1_xdg), &pos2, &qh, 2u8);
    p2_popup.grab(&seat, 0);
    app.p2_xdg = Some(p2_xdg);
    app.p2_popup = Some(p2_popup);
    p2_surface.commit();
    let submenu = pump_until(&mut queue, &mut app, &h.captures, 5, is_p2)
        .expect("submenu popup never composited");
    let boundary = submenu.serial;
    save_composited(
        "popup_nested_chain-open",
        TL_W,
        TL_H,
        TL,
        &[(&submenu, 100, 100)],
    );

    // ---- press OUTSIDE the whole chain → both dismissed, innermost first ----
    h.input_tx
        .send(InputCommand::PointerMotion { x: 5.0, y: 5.0 })
        .expect("motion outside");
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("press");
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.dismiss_order.len() == 2),
        "both popups never received popup_done (got {:?})",
        app.dismiss_order,
    );
    assert_eq!(
        app.dismiss_order,
        vec![2, 1],
        "popup_done arrived innermost-first (submenu 2 before menu 1)"
    );
    let _ = queue.roundtrip(&mut app);

    // ---- both popups gone from the composited output ----
    let after = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > boundary && is_tl(f)
    })
    .expect("toplevel never re-presented after the chain dismissed");
    let caps = h.captures.lock().unwrap();
    assert!(
        !caps.iter().any(|f| f.serial > boundary && is_p1(f)),
        "menu popup was still composited after dismissal"
    );
    assert!(
        !caps.iter().any(|f| f.serial > boundary && is_p2(f)),
        "submenu popup was still composited after dismissal"
    );
    drop(caps);
    save_composited(
        "popup_nested_chain-closed",
        TL_W,
        TL_H,
        TL,
        &[(&after, 0, 0)],
    );

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
impl Dispatch<XdgSurface, Role> for App {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        role: &Role,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
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
                Role::Popup1 if !app.p1_drawn => {
                    app.p1_surface.attach(Some(&app.p1_buffer), 0, 0);
                    app.p1_surface.damage(0, 0, P1_W, P1_H);
                    app.p1_surface.commit();
                    app.p1_drawn = true;
                }
                Role::Popup2 if !app.p2_drawn => {
                    app.p2_surface.attach(Some(&app.p2_buffer), 0, 0);
                    app.p2_surface.damage(0, 0, P2_W, P2_H);
                    app.p2_surface.commit();
                    app.p2_drawn = true;
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<XdgPopup, u8> for App {
    fn event(
        app: &mut Self,
        _: &XdgPopup,
        e: <XdgPopup as Proxy>::Event,
        id: &u8,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = e {
            app.dismiss_order.push(*id);
            // Tear down this popup in protocol order.
            if *id == 2 {
                if let Some(p) = app.p2_popup.take() {
                    p.destroy();
                }
                if let Some(x) = app.p2_xdg.take() {
                    x.destroy();
                }
                app.p2_surface.destroy();
            } else {
                if let Some(p) = app.p1_popup.take() {
                    p.destroy();
                }
                if let Some(x) = app.p1_xdg.take() {
                    x.destroy();
                }
                app.p1_surface.destroy();
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
    WlSeat,
    XdgToplevel,
    XdgPositioner
);
