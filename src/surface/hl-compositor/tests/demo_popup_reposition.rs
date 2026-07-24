//! DEMO — `popup_reposition` (a mapped popup is re-anchored; the client is reconfigured and it MOVES).
//!
//! `xdg_popup.reposition` (xdg-shell v3) lets a mapped popup be re-placed without being destroyed — a
//! menu re-anchoring as the pointer walks a menu bar. The test maps a toplevel, opens a popup at
//! positioner P1 (resolving to on-screen offset EXPECT1), then calls `reposition(P2, token)`. It asserts:
//!
//!   * the client receives `xdg_popup.repositioned(token)` echoing the exact token;
//!   * followed by a fresh `xdg_popup.configure(x, y, w, h)` at the NEW resolved offset EXPECT2;
//!   * after the client acks + re-commits, the popup is composited at EXPECT2 and NO LONGER at EXPECT1 —
//!     it actually moved.
//!
//! Covers the reposition request AND its `repositioned` reply, closed on composited pixels.

mod client_harness;
use client_harness::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use hl_compositor::adapter::smithay::CapturedFrame;

const TL_W: i32 = 320;
const TL_H: i32 = 240;
const TL: [u8; 4] = [0x20, 0x20, 0x50, 0xFF];
const POP_W: i32 = 48;
const POP_H: i32 = 36;
const POP: [u8; 4] = [0xF0, 0x80, 0x10, 0xFF];

// BottomLeft anchor + BottomRight gravity ⇒ x = anchor.x + off.x, y = anchor.y + anchor.h + off.y.
const A1: (i32, i32, i32, i32) = (50, 50, 20, 20);
const O1: (i32, i32) = (10, 10);
const EXPECT1: (i32, i32) = (60, 80); // 50+10, 50+20+10

const A2: (i32, i32, i32, i32) = (150, 120, 10, 10);
const O2: (i32, i32) = (5, 6);
const EXPECT2: (i32, i32) = (155, 136); // 150+5, 120+10+6

const TOKEN: u32 = 4242;

#[derive(Clone, Copy, PartialEq)]
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
    /// Latest `xdg_popup.configure(x, y, w, h)`.
    pop_configure: Option<(i32, i32, i32, i32)>,
    /// Received `xdg_popup.repositioned` token, if any.
    repositioned_token: Option<u32>,
}

fn is_pop_at(f: &CapturedFrame, at: (i32, i32)) -> bool {
    f.width == POP_W
        && f.height == POP_H
        && (f.x, f.y) == at
        && f.pixel_is(POP_W / 2, POP_H / 2, POP)
}
fn is_tl(f: &CapturedFrame) -> bool {
    f.width == TL_W && f.height == TL_H && f.pixel_is(TL_W / 2, TL_H / 2, TL)
}

#[test]
fn popup_reposition() {
    let h = Harness::start("popup_reposition");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 3..=6, ()).expect("xdg_wm_base v3+");

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
    toplevel.set_title("demo-reposition".into());
    tl_surface.commit();
    let pop_surface = compositor.create_surface(&qh, ());

    let mut app = App {
        tl_surface: tl_surface.clone(),
        tl_buffer,
        tl_drawn: false,
        tl_frame_done: false,
        pop_surface: pop_surface.clone(),
        pop_buffer,
        pop_configure: None,
        repositioned_token: None,
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

    // ---- open the popup at positioner P1 ----
    let pos1: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos1.set_size(POP_W, POP_H);
    pos1.set_anchor_rect(A1.0, A1.1, A1.2, A1.3);
    pos1.set_anchor(xdg_positioner::Anchor::BottomLeft);
    pos1.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos1.set_offset(O1.0, O1.1);
    let pop_xdg = wm_base.get_xdg_surface(&pop_surface, &qh, Role::Popup);
    let popup: XdgPopup = pop_xdg.get_popup(Some(&tl_xdg), &pos1, &qh, ());
    pop_surface.commit();

    let first = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        is_pop_at(f, EXPECT1)
    })
    .expect("popup never composited at EXPECT1");
    assert_eq!(
        (first.x, first.y),
        EXPECT1,
        "popup opened at the P1-resolved offset"
    );
    assert_eq!(
        app.pop_configure.map(|(x, y, ..)| (x, y)),
        Some(EXPECT1),
        "initial xdg_popup.configure carried EXPECT1"
    );
    save_composited(
        "popup_reposition-before",
        TL_W,
        TL_H,
        TL,
        &[(&first, EXPECT1.0, EXPECT1.1)],
    );
    let boundary = first.serial;

    // ---- reposition to P2 ----
    let pos2: XdgPositioner = wm_base.create_positioner(&qh, ());
    pos2.set_size(POP_W, POP_H);
    pos2.set_anchor_rect(A2.0, A2.1, A2.2, A2.3);
    pos2.set_anchor(xdg_positioner::Anchor::BottomLeft);
    pos2.set_gravity(xdg_positioner::Gravity::BottomRight);
    pos2.set_offset(O2.0, O2.1);
    popup.reposition(&pos2, TOKEN);

    // The client receives repositioned(token) + a fresh configure at EXPECT2 (the xdg_surface handler acks
    // and re-commits the buffer), and the popup re-composites at EXPECT2.
    let moved = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.serial > boundary && is_pop_at(f, EXPECT2)
    })
    .expect("popup never re-composited at EXPECT2 after reposition");
    assert_eq!(
        app.repositioned_token,
        Some(TOKEN),
        "client received xdg_popup.repositioned with the exact token"
    );
    assert_eq!(
        app.pop_configure.map(|(x, y, w, hh)| (x, y, w, hh)),
        Some((EXPECT2.0, EXPECT2.1, POP_W, POP_H)),
        "reposition configure carried the NEW offset + size"
    );
    assert_eq!(
        (moved.x, moved.y),
        EXPECT2,
        "popup composited at the P2-resolved offset"
    );

    // The popup is no longer composited at EXPECT1 after the move.
    let still_old = h
        .captures
        .lock()
        .unwrap()
        .iter()
        .any(|f| f.serial > boundary && is_pop_at(f, EXPECT1));
    assert!(
        !still_old,
        "popup was still composited at the OLD offset after reposition"
    );
    save_composited(
        "popup_reposition-after",
        TL_W,
        TL_H,
        TL,
        &[(&moved, EXPECT2.0, EXPECT2.1)],
    );

    let _ = (popup, pop_xdg);
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
                Role::Popup => {
                    // Re-attach + commit on EVERY popup configure (initial and post-reposition) so the new
                    // geometry takes effect.
                    app.pop_surface.attach(Some(&app.pop_buffer), 0, 0);
                    app.pop_surface.damage(0, 0, POP_W, POP_H);
                    app.pop_surface.commit();
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
        match e {
            xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => app.pop_configure = Some((x, y, width, height)),
            xdg_popup::Event::Repositioned { token } => app.repositioned_token = Some(token),
            _ => {}
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
    XdgToplevel,
    XdgPositioner
);
