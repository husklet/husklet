//! DEMO 2 — `popup_dismiss_on_outside_click`.
//!
//! With a GRABBED popup up, injecting a pointer press OUTSIDE the popup chain must (a) deliver
//! `xdg_popup.popup_done` to the client AND (b) make the popup disappear VISUALLY: the next composited
//! frame no longer contains the popup layer. This exercises the compositor end to end — the grab
//! dismissal AND the re-present that removes the dismissed popup from the screen without the client
//! having to repaint its toplevel.

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

use hl_compositor::adapter::smithay::InputCommand;

const TL_W: i32 = 300;
const TL_H: i32 = 200;
const TL: [u8; 4] = [0x20, 0x20, 0xC0, 0xFF]; // blue

const POP_W: i32 = 40;
const POP_H: i32 = 30;
const POP: [u8; 4] = [0xE0, 0x10, 0x10, 0xFF]; // red
const ANCHOR: (i32, i32, i32, i32) = (100, 100, 20, 20);
const OFFSET: (i32, i32) = (7, 9);
const EXPECT_POP: (i32, i32) = (107, 129);

const BTN_LEFT: u32 = 0x110;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Toplevel,
    Popup,
}

struct App {
    tl_surface: WlSurface,
    tl_buffer: WlBuffer,
    tl_drawn: bool,
    tl_frame_done: bool,
    pop_surface: WlSurface,
    pop_buffer: WlBuffer,
    pop_drawn: bool,
    // popup proxies kept so we can tear the popup down on dismissal
    popup: Option<XdgPopup>,
    pop_xdg: Option<XdgSurface>,
    pop_done: bool,
    pop_destroyed: bool,
}

/// Does a captured frame look like the popup layer (its exact size, placement, and color)?
fn is_popup_layer(f: &hl_compositor::adapter::smithay::CapturedFrame) -> bool {
    f.width == POP_W
        && f.height == POP_H
        && (f.x, f.y) == EXPECT_POP
        && f.pixel_is(POP_W / 2, POP_H / 2, POP)
}
fn is_toplevel_layer(f: &hl_compositor::adapter::smithay::CapturedFrame) -> bool {
    f.width == TL_W && f.height == TL_H && f.pixel_is(TL_W / 2, TL_H / 2, TL)
}

#[test]
fn popup_dismiss_on_outside_click() {
    let h = Harness::start("popup_dismiss");

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
    let pop_buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "pop",
        POP_W,
        POP_H,
        &solid(POP_W, POP_H, POP),
    );

    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, Role::Toplevel);
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-dismiss".into());
    tl_surface.commit();
    let pop_surface = compositor.create_surface(&qh, ());

    let mut app = App {
        tl_surface: tl_surface.clone(),
        tl_buffer,
        tl_drawn: false,
        tl_frame_done: false,
        pop_surface: pop_surface.clone(),
        pop_buffer,
        pop_drawn: false,
        popup: None,
        pop_xdg: None,
        pop_done: false,
        pop_destroyed: false,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_frame_done) {
        assert!(Instant::now() < deadline, "toplevel never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    assert!(
        pump_until(&mut queue, &mut app, &h.captures, 5, is_toplevel_layer).is_some(),
        "toplevel never composited",
    );

    // ---- open the grabbed popup ----
    let pos: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos.set_size(POP_W, POP_H);
    pos.set_anchor_rect(ANCHOR.0, ANCHOR.1, ANCHOR.2, ANCHOR.3);
    pos.set_anchor(xdg_positioner::Anchor::BottomLeft);
    pos.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos.set_offset(OFFSET.0, OFFSET.1);
    let pop_xdg = wm_base.get_xdg_surface(&pop_surface, &qh, Role::Popup);
    let popup: XdgPopup = pop_xdg.get_popup(Some(&tl_xdg), &pos, &qh, ());
    popup.grab(&seat, 0);
    app.popup = Some(popup);
    app.pop_xdg = Some(pop_xdg);
    pop_surface.commit();

    let popup_frame = pump_until(&mut queue, &mut app, &h.captures, 5, is_popup_layer)
        .expect("popup never composited before dismiss");
    // The serial of the last present that still SHOWED the popup — the "before" boundary.
    let showed_popup_serial = popup_frame.serial;
    save_composited(
        "popup_dismiss-frame0-open",
        TL_W,
        TL_H,
        TL,
        &[(&popup_frame, EXPECT_POP.0, EXPECT_POP.1)],
    );

    // ---- click OUTSIDE the popup rect -> grab dismissed ----
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
        pump_while(&mut queue, &mut app, 5, |a| a.pop_done),
        "client never received xdg_popup.popup_done on the outside press",
    );

    // The popup_done handler tears the popup down (destroy popup/xdg_surface/wl_surface). Pump so the
    // server processes those destroys.
    assert!(
        pump_while(&mut queue, &mut app, 5, |a| a.pop_destroyed),
        "client never destroyed the popup"
    );
    let _ = queue.roundtrip(&mut app);

    // ---- assert the popup is VISUALLY gone: a fresh toplevel-only frame presents, and no popup layer
    // is ever composited again after the dismiss ----
    let after = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > showed_popup_serial && is_toplevel_layer(f)
    })
    .expect("compositor never re-presented the toplevel after the popup was dismissed (popup left on screen)");

    // No popup layer composited after the boundary — the popup is gone, not merely redrawn behind.
    let popup_after = h
        .captures
        .lock()
        .unwrap()
        .iter()
        .any(|f| f.serial > showed_popup_serial && is_popup_layer(f));
    assert!(
        !popup_after,
        "a popup layer was still composited after dismissal — popup did not disappear"
    );

    save_composited(
        "popup_dismiss-frame1-closed",
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
                Role::Popup if !app.pop_drawn => {
                    app.pop_surface.attach(Some(&app.pop_buffer), 0, 0);
                    app.pop_surface.damage(0, 0, POP_W, POP_H);
                    app.pop_surface.commit();
                    app.pop_drawn = true;
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<XdgPopup, ()> for App {
    fn event(
        app: &mut Self,
        _: &XdgPopup,
        e: <XdgPopup as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = e {
            app.pop_done = true;
            // Tear the popup down in protocol order: xdg_popup -> xdg_surface -> wl_surface.
            if let Some(popup) = app.popup.take() {
                popup.destroy();
            }
            if let Some(xdg) = app.pop_xdg.take() {
                xdg.destroy();
            }
            app.pop_surface.destroy();
            app.pop_destroyed = true;
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
