//! DEMO — `drag_and_drop` (a real `wl_data_device` DnD: source drags an offer over a target, target reads
//! the dragged bytes over a real fd).
//!
//! The drag-and-drop path Chrome/GTK need. A SOURCE client offers a mime on a `wl_data_source` and starts a
//! drag (in response to a pointer button press, whose serial anchors the implicit grab Smithay turns into
//! its DnD grab). The host then drives the drag pointer via the ordinary pointer seam ([`InputCommand::
//! PointerMotion`] / [`InputCommand::PointerButton`], which now route through the DnD grab). Asserted, in
//! order, on a TARGET client's `wl_data_device`:
//!
//!   * `data_offer` advertising the source's mime + `source_actions`, then `enter` at the EXACT surface-local
//!     coordinate the pointer hit;
//!   * `motion` as the pointer moves (coords update);
//!   * `leave` when the pointer moves OFF the target (a cancel), then `enter` again on moving back;
//!   * `drop` (only because the target NEGOTIATED it — accepted the mime + a Copy action), after which the
//!     target `receive`s the dragged bytes over a real pipe and reads EXACTLY the source's payload.
//!
//! The source + target are DISTINCT sizes (small over large, both rooted at the origin) so a point past the
//! source's width hit-tests unambiguously to the target — the neutral scene models no window position, so
//! size is how the drag is steered from one surface to the other. `Observations::dnd_active` /
//! `dnd_drop_validated` (the SOURCE-side grab lifecycle, which emits no client wire event) gate + confirm.
//!
//! Before this was wired the headless adapter never produced DnD enter/motion/drop — nothing to assert.

mod client_harness;
use client_harness::*;

use std::io::Read;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::InputCommand;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_pointer::{self, ButtonState, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const MIME: &str = "text/plain;charset=utf-8";
const PAYLOAD: &[u8] = b"\x07\xA5\x11 hl drag payload";
const BTN_LEFT: u32 = 0x110;
// Source A is SMALL; target B is LARGE. A point with x >= A_W (or y >= A_H) is only inside B, so the drag
// steers there unambiguously despite both toplevels rooting at (0, 0).
const A_W: i32 = 120;
const A_H: i32 = 100;
const B_W: i32 = 240;
const B_H: i32 = 180;
// Drag path over the target (each strictly outside A: x >= A_W).
const START: (i32, i32) = (30, 30); // inside A (the drag origin / button press)
const T1: (i32, i32) = (170, 60);
const T2: (i32, i32) = (200, 120);
const OUT: (i32, i32) = (400, 400); // outside every surface (cancel → leave)
const T3: (i32, i32) = (150, 90); // re-enter (x >= A_W, y < B_H)

#[derive(Debug, Clone, Copy, PartialEq)]
enum DndEv {
    Enter(i32, i32),
    Motion(i32, i32),
    Leave,
    Drop,
}

#[derive(Default)]
struct App {
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
    drawn: bool,
    frame_done: bool,
    // ---- source (A) ----
    dd: Option<WlDataDevice>,
    source: Option<WlDataSource>,
    source_send_fired: bool,
    // ---- target (B) ----
    events: Vec<DndEv>,
    offered_mimes: Vec<String>,
    source_actions: Option<DndAction>,
    current_offer: Option<WlDataOffer>,
    received: Vec<u8>,
}

impl App {
    fn enters(&self) -> Vec<(i32, i32)> {
        self.events
            .iter()
            .filter_map(|e| {
                if let DndEv::Enter(x, y) = e {
                    Some((*x, *y))
                } else {
                    None
                }
            })
            .collect()
    }
    fn motions(&self) -> Vec<(i32, i32)> {
        self.events
            .iter()
            .filter_map(|e| {
                if let DndEv::Motion(x, y) = e {
                    Some((*x, *y))
                } else {
                    None
                }
            })
            .collect()
    }
}

fn spawn(
    dir: &std::path::Path,
    tag: &str,
    w: i32,
    h: i32,
) -> (
    Connection,
    EventQueue<App>,
    App,
    WlDataDeviceManager,
    WlSeat,
    QueueHandle<App>,
) {
    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat");
    let ddm: WlDataDeviceManager = globals
        .bind(&qh, 3..=3, ())
        .expect("wl_data_device_manager v3");

    let buffer = make_buffer(
        &shm,
        &qh,
        dir,
        tag,
        w,
        h,
        &solid(w, h, [0x40, 0x50, 0x60, 0xFF]),
    );
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("demo-dnd-{tag}"));
    surface.commit();

    let mut app = App {
        surface: Some(surface.clone()),
        buffer: Some(buffer),
        ..App::default()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "client {tag} never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = queue.roundtrip(&mut app);
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    (conn, queue, app, ddm, seat, qh)
}

#[test]
fn drag_and_drop() {
    let h = Harness::start("drag_and_drop");

    // A = small source, B = large target (both root at origin; size disambiguates the hit-test).
    let (_ca, mut qa, mut a, ddm_a, seat_a, qha) = spawn(&h.runtime_dir, "A", A_W, A_H);
    let (_cb, mut qb, mut b, ddm_b, seat_b, qhb) = spawn(&h.runtime_dir, "B", B_W, B_H);
    // Wait until B's larger frame is the one on top so hit-testing over the target region is live.
    assert!(
        pump_until(&mut qb, &mut b, &h.captures, 5, |f| f.width == B_W).is_some(),
        "target B frame never composited",
    );

    // A: data device + a source offering our mime + a Copy DnD action.
    let dd_a: WlDataDevice = ddm_a.get_data_device(&seat_a, &qha, ());
    let source: WlDataSource = ddm_a.create_data_source(&qha, ());
    source.offer(MIME.to_string());
    source.set_actions(DndAction::Copy);
    a.dd = Some(dd_a);
    a.source = Some(source);
    // B: a data device to receive the drag.
    let _dd_b: WlDataDevice = ddm_b.get_data_device(&seat_b, &qhb, ());
    // A needs a pointer so an implicit grab + button serial exist to anchor start_drag.
    let _ptr_a: WlPointer = seat_a.get_pointer(&qha, ());
    let _ = qa.roundtrip(&mut a);
    let _ = qb.roundtrip(&mut b);

    // Focus A so the start point (inside both toplevels) hit-tests to A, then enter A + press BTN_LEFT.
    // A's pointer-button handler calls start_drag with the button serial.
    h.input_tx
        .send(InputCommand::FocusToplevelIndex(0))
        .expect("focus A");
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: START.0 as f64,
            y: START.1 as f64,
        })
        .expect("enter A");
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .expect("press");

    // The DnD grab goes live once A's start_drag is honoured (SOURCE-side: no client wire event, so observe).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !h.observations.lock().unwrap().dnd_active {
        assert!(
            Instant::now() < deadline,
            "DnD grab never became active (start_drag not honoured)"
        );
        let _ = qa.roundtrip(&mut a);
        std::thread::sleep(Duration::from_millis(5));
    }

    // ---- drag over the target: enter + motion at exact surface-local coords ----
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: T1.0 as f64,
            y: T1.1 as f64,
        })
        .expect("drag→T1");
    assert!(
        pump_while(&mut qb, &mut b, 5, |b| b.enters().contains(&T1)),
        "target never got drag `enter` at {T1:?}; events={:?}",
        b.events,
    );
    assert!(
        b.offered_mimes.contains(&MIME.to_string()),
        "offer advertised the source mime, got {:?}",
        b.offered_mimes
    );
    assert_eq!(
        b.source_actions,
        Some(DndAction::Copy),
        "offer advertised the source's Copy action"
    );

    h.input_tx
        .send(InputCommand::PointerMotion {
            x: T2.0 as f64,
            y: T2.1 as f64,
        })
        .expect("drag→T2");
    assert!(
        pump_while(&mut qb, &mut b, 5, |b| b.motions().contains(&T2)),
        "target never got drag `motion` at {T2:?}; events={:?}",
        b.events,
    );

    // ---- cancel: move OFF the target → `leave` ----
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: OUT.0 as f64,
            y: OUT.1 as f64,
        })
        .expect("drag→OUT");
    assert!(
        pump_while(&mut qb, &mut b, 5, |b| b.events.contains(&DndEv::Leave)),
        "target never got drag `leave` on moving off; events={:?}",
        b.events,
    );

    // ---- move back on → `enter` again (a fresh offer) ----
    h.input_tx
        .send(InputCommand::PointerMotion {
            x: T3.0 as f64,
            y: T3.1 as f64,
        })
        .expect("drag→T3");
    assert!(
        pump_while(&mut qb, &mut b, 5, |b| b.enters().contains(&T3)),
        "target never re-entered at {T3:?}; events={:?}",
        b.events,
    );
    // Flush B's `accept` + `set_actions` on the re-enter offer to the server (and sync past them) BEFORE the
    // release, so the drop is NEGOTIATED (accepted mime + non-empty Copy action) rather than cancelled.
    let _ = qb.roundtrip(&mut b);

    // ---- drop: release the button over the target ----
    h.input_tx
        .send(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .expect("release");
    assert!(
        pump_while(&mut qb, &mut b, 5, |b| b.events.contains(&DndEv::Drop)),
        "target never got `drop`; events={:?}",
        b.events,
    );

    // ---- the target reads the dragged bytes over a real pipe ----
    let offer = b
        .current_offer
        .clone()
        .expect("target holds a data offer at drop");
    let (mut reader, writer) = UnixStream::pair().expect("pipe pair");
    offer.receive(MIME.to_string(), writer.as_fd());
    let _ = qb.roundtrip(&mut b);
    drop(writer);
    reader.set_nonblocking(true).expect("nonblocking reader");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = qa.roundtrip(&mut a);
        let _ = qb.roundtrip(&mut b);
        let mut tmp = [0u8; 256];
        match reader.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => b.received.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("read from drag pipe failed: {e}"),
        }
        assert!(
            Instant::now() < deadline,
            "dragged bytes never arrived; got {:?}",
            b.received
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        b.received, PAYLOAD,
        "target received EXACTLY the source's dragged bytes"
    );
    assert!(a.source_send_fired, "source's wl_data_source.send fired");

    // The SOURCE-side grab lifecycle: the drop was reached AND negotiated (validated).
    let obs = h.observations.lock().unwrap().clone();
    assert!(obs.dnd_dropped, "the DnD reached its drop");
    assert!(
        obs.dnd_drop_validated,
        "the drop was negotiated (accepted mime + Copy action)"
    );
    assert!(!obs.dnd_active, "the DnD grab is released after the drop");

    // ---- the delivered wire sequence, in order ----
    // enter(T1) before motion(T2) before leave before enter(T3) before drop.
    let idx_enter1 = b
        .events
        .iter()
        .position(|e| *e == DndEv::Enter(T1.0, T1.1))
        .unwrap();
    let idx_motion = b
        .events
        .iter()
        .position(|e| *e == DndEv::Motion(T2.0, T2.1))
        .unwrap();
    let idx_leave = b.events.iter().position(|e| *e == DndEv::Leave).unwrap();
    let idx_enter2 = b
        .events
        .iter()
        .position(|e| *e == DndEv::Enter(T3.0, T3.1))
        .unwrap();
    let idx_drop = b.events.iter().position(|e| *e == DndEv::Drop).unwrap();
    assert!(
        idx_enter1 < idx_motion
            && idx_motion < idx_leave
            && idx_leave < idx_enter2
            && idx_enter2 < idx_drop,
        "DnD events in order [enter,motion,leave,enter,drop]; got {:?}",
        b.events,
    );

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

#[path = "compositor/demo_drag_and_drop.rs"]
mod dispatch;
