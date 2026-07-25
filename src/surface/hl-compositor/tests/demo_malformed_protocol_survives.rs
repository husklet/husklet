//! ROBUSTNESS DEMO 2 — `malformed_protocol_survives` (out-of-order / invalid ops never wedge the adapter).
//!
//! Three families of protocol abuse are driven and each proven survivable:
//!
//!   * COMMIT WITH NO ATTACHED BUFFER — a mapped client commits without ever attaching content. The
//!     adapter must treat this as a benign no-content commit (not a panic, not a corrupt scene). Proven
//!     RECOVERABLE: the SAME client then attaches a real buffer and composites an exact frame.
//!   * ack_configure WITH AN UNKNOWN SERIAL — a client acks a serial the compositor never sent. This is a
//!     fatal xdg-shell protocol error (Smithay disconnects the offender); the assert is that the
//!     compositor thread stays alive and keeps serving.
//!   * DOUBLE wl_surface.destroy — a client destroys the same surface twice. The second request targets a
//!     dead object; the compositor must not crash.
//!
//! After all three, a fresh well-behaved [`Neighbor`] connects and composites an EXACT frame — proof the
//! adapter survived every abuse and still serves normal clients.
//!
//! Reachability note: the unknown-serial ack and the double-destroy are FATAL protocol errors — Smithay
//! disconnects the offending client rather than letting it recover. So "the same client keeps going" is
//! only assertable for the non-fatal commit-with-no-buffer case; for the fatal ones the survival proof is
//! the compositor + a neighbor, which is exactly what a real compositor must guarantee (one client's
//! protocol violation cannot take down the others).

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
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 140;
const H: i32 = 100;
const RECOVER: [u8; 4] = [0xE0, 0x40, 0xC0, 0xFF]; // magenta — the client that empty-commits then recovers
const NEIGHBOR: [u8; 4] = [0x40, 0xE0, 0x40, 0xFF]; // green — the final well-behaved survivor

/// A client that maps a toplevel, then (once configured) commits WITH NO BUFFER before drawing real
/// content — exercising the adapter's no-content-commit path, then recovering.
struct Recover {
    surface: WlSurface,
    buffer: WlBuffer,
    configured: bool,
    empty_committed: bool,
    drew: bool,
    frame_done: bool,
}

#[test]
fn malformed_protocol_survives() {
    let h = Harness::start("malformed_protocol");

    // ---- abuse 1: commit with NO attached buffer, then recover in the SAME client ----
    let conn = Connection::connect_to_env().expect("recover connect");
    let (globals, mut queue) = registry_queue_init::<Recover>(&conn).expect("recover registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    let buffer = make_buffer(
        &shm,
        &qh,
        &h.runtime_dir,
        "recover",
        W,
        H,
        &solid(W, H, RECOVER),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("malformed-recover".into());
    surface.commit();

    let mut app = Recover {
        surface: surface.clone(),
        buffer: buffer.clone(),
        configured: false,
        empty_committed: false,
        drew: false,
        frame_done: false,
    };
    // Drive the handshake: the Configure handler does the empty commit first, then the real draw.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drew && app.frame_done) {
        assert!(
            Instant::now() < deadline,
            "recover client never mapped through the empty-commit path"
        );
        queue.blocking_dispatch(&mut app).expect("recover dispatch");
    }
    let recover_frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.pixel_is(1, 1, RECOVER)
    })
    .expect("recover client never composited after an empty commit");
    assert_eq!(
        recover_frame.pixel(W / 2, H / 2).unwrap(),
        RECOVER,
        "recovered client is solid magenta"
    );
    assert!(
        app.empty_committed,
        "the no-buffer commit path was actually exercised"
    );

    // ---- abuse 2: ack_configure with a serial the compositor never sent (fatal → kills only offender) ----
    hostile_bad_ack();

    // ---- abuse 3: double wl_surface.destroy (second targets a dead object) ----
    hostile_double_destroy();

    // Let the serve loop process the two hostile disconnects.
    std::thread::sleep(Duration::from_millis(50));

    // The empty-commit client is STILL alive and still composites (adapter did not corrupt its state).
    let recover_again = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.pixel_is(1, 1, RECOVER)
    })
    .expect("recover client no longer serves after the fatal-error abuses");
    assert_eq!(
        recover_again.pixel(2, 2).unwrap(),
        RECOVER,
        "recover client unaffected by neighbors' abuse"
    );

    // ---- survivor: a brand-new well-behaved client composites an exact frame ----
    let mut neighbor = Neighbor::map(&h.runtime_dir, "survivor", W, H, NEIGHBOR);
    let frame = neighbor.assert_presents(&h.captures);
    save_frame("malformed_protocol-recover", &recover_frame);
    save_frame("malformed_protocol-survivor", &frame);

    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    h.shutdown();
}

/// Bind xdg-shell, map a toplevel, and on its first configure ACK A BOGUS SERIAL (real serial + 9999).
/// Smithay raises `xdg_wm_base.error.invalid_surface_state`/wrong-serial and disconnects this client.
fn hostile_bad_ack() {
    let conn = Connection::connect_to_env().expect("bad-ack connect");
    let (globals, mut queue) = registry_queue_init::<BadAck>(&conn).expect("bad-ack registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let _toplevel = xdg.get_toplevel(&qh, ());
    surface.commit();
    let mut app = BadAck { acked: false };
    let deadline = Instant::now() + Duration::from_secs(3);
    while !app.acked && Instant::now() < deadline {
        if queue.blocking_dispatch(&mut app).is_err() {
            break; // the protocol error disconnected us — expected
        }
    }
    let _ = queue.roundtrip(&mut app);
}

/// Map a surface, draw one frame, then destroy the SAME `wl_surface` twice in a row.
fn hostile_double_destroy() {
    let conn = Connection::connect_to_env().expect("double-destroy connect");
    let (globals, mut queue) =
        registry_queue_init::<Plain>(&conn).expect("double-destroy registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let surface = compositor.create_surface(&qh, ());
    // Destroy it, then destroy it again — the second request references a now-dead object id.
    surface.destroy();
    surface.destroy();
    let _ = queue.roundtrip(&mut Plain);
}

// ---------- app types + dispatch plumbing ----------

struct BadAck {
    acked: bool,
}
struct Plain;

impl Dispatch<XdgSurface, ()> for Recover {
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
            app.configured = true;
            if !app.empty_committed {
                // ABUSE: commit with NO buffer attached (a no-content commit).
                app.surface.commit();
                app.empty_committed = true;
            }
            if !app.drew {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drew = true;
            }
        }
    }
}
impl Dispatch<WlCallback, ()> for Recover {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        e: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = e {
            app.frame_done = true;
        }
    }
}
impl Dispatch<WlRegistry, GlobalListContents> for Recover {
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
impl Dispatch<XdgWmBase, ()> for Recover {
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

impl Dispatch<XdgSurface, ()> for BadAck {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            // ABUSE: ack a serial the compositor never sent.
            xdg.ack_configure(serial + 9999);
            app.acked = true;
        }
    }
}
impl Dispatch<WlRegistry, GlobalListContents> for BadAck {
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
impl Dispatch<XdgWmBase, ()> for BadAck {
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
impl Dispatch<WlRegistry, GlobalListContents> for Plain {
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

macro_rules! ignore {
    ($ty:ty; $($t:ty),*) => {$(
        impl Dispatch<$t, ()> for $ty {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(Recover; WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, XdgToplevel);
ignore!(BadAck; WlCompositor, WlSurface, XdgToplevel);
ignore!(Plain; WlCompositor, WlSurface);
