//! DEMO 10 — `clipboard_selection_roundtrip` (a real cross-client copy/paste over a live fd).
//!
//! TWO independent clients. Client A creates a `wl_data_source`, offers a mime type, and — while it holds
//! keyboard focus — sets it as the clipboard selection. Focus then moves to client B; B's
//! `wl_data_device` receives a `data_offer` (advertising A's mime) followed by `selection`. B opens a
//! pipe, calls `wl_data_offer.receive(mime, fd)`, and reads. The compositor routes the `receive` back to
//! A as `wl_data_source.send(mime, fd)`; A writes its bytes; B reads the EXACT payload back off the pipe.
//!
//! This is a genuine end-to-end clipboard flow across two Wayland clients — the offered mime round-trips,
//! and the byte sequence A "copied" is exactly what B "pastes". (The in-tree comment claiming live paste
//! is out of scope understates the adapter: Smithay forwards the client-source fd natively; this proves
//! it works through the hl adapter's focus-follows-selection wiring.)

mod common;
use common::*;

use std::io::{Read, Write};
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
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_keyboard::{self, WlKeyboard},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const W: i32 = 140;
const H: i32 = 100;
const MIME: &str = "text/plain;charset=utf-8";
const PAYLOAD: &[u8] = b"hl clipboard roundtrip \xE2\x9C\x93 42";

struct App {
    surface: WlSurface,
    buffer: WlBuffer,
    drawn: bool,
    frame_done: bool,
    // ---- source side (A) ----
    /// The keyboard-enter serial A uses to set the selection.
    kbd_enter_serial: Option<u32>,
    /// A's `wl_data_source.send` fired and A wrote the payload.
    source_send_fired: bool,
    // ---- receiver side (B) ----
    /// Mime types advertised on any received `wl_data_offer`.
    offered_mimes: Vec<String>,
    /// The offer delivered by `wl_data_device.selection` (the current clipboard).
    selection_offer: Option<WlDataOffer>,
}

fn spawn(
    dir: &std::path::Path,
    tag: &str,
    color: [u8; 4],
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
        .bind(&qh, 1..=3, ())
        .expect("wl_data_device_manager");

    let buffer = make_buffer(&shm, &qh, dir, tag, W, H, &solid(W, H, color));
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(format!("demo-clip-{tag}"));
    let _kbd: WlKeyboard = seat.get_keyboard(&qh, ());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer,
        drawn: false,
        frame_done: false,
        kbd_enter_serial: None,
        source_send_fired: false,
        offered_mimes: Vec::new(),
        selection_offer: None,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.drawn && app.frame_done) {
        assert!(Instant::now() < deadline, "client {tag} never mapped");
        queue.blocking_dispatch(&mut app).expect("dispatch map");
    }
    let _ = queue.roundtrip(&mut app);
    std::mem::forget(toplevel);
    std::mem::forget(xdg);
    std::mem::forget(_kbd);
    (conn, queue, app, ddm, seat, qh)
}

#[test]
fn clipboard_selection_roundtrip() {
    let h = Harness::start("clipboard");

    // A maps first (index 0), B second (index 1).
    let (_ca, mut qa, mut a, ddm_a, seat_a, qha) =
        spawn(&h.runtime_dir, "A", [0xC0, 0x40, 0x40, 0xFF]);
    let (_cb, mut qb, mut b, ddm_b, seat_b, qhb) =
        spawn(&h.runtime_dir, "B", [0x40, 0x40, 0xC0, 0xFF]);

    // A: a data device + a source offering our mime.
    let dd_a: WlDataDevice = ddm_a.get_data_device(&seat_a, &qha, ());
    let source: WlDataSource = ddm_a.create_data_source(&qha, ());
    source.offer(MIME.to_string());
    // B: a data device (its data_offer children get udata () via event_created_child below).
    let _dd_b: WlDataDevice = ddm_b.get_data_device(&seat_b, &qhb, ());
    let _ = qa.roundtrip(&mut a);
    let _ = qb.roundtrip(&mut b);

    // ---- A takes focus and sets the selection (requires keyboard focus) ----
    h.input_tx
        .send(InputCommand::FocusToplevelIndex(0))
        .expect("focus A");
    let deadline = Instant::now() + Duration::from_secs(5);
    while a.kbd_enter_serial.is_none() {
        assert!(Instant::now() < deadline, "A never received keyboard focus");
        let _ = qa.roundtrip(&mut a);
        std::thread::sleep(Duration::from_millis(5));
    }
    let serial = a.kbd_enter_serial.unwrap();
    dd_a.set_selection(Some(&source), serial);
    let _ = qa.roundtrip(&mut a);

    // ---- focus moves to B → B's data device gets the offer + selection ----
    h.input_tx
        .send(InputCommand::FocusToplevelIndex(1))
        .expect("focus B");
    let deadline = Instant::now() + Duration::from_secs(5);
    while b.selection_offer.is_none() {
        assert!(
            Instant::now() < deadline,
            "B never received a selection offer; mimes seen: {:?}",
            b.offered_mimes
        );
        let _ = qa.roundtrip(&mut a);
        let _ = qb.roundtrip(&mut b);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        b.offered_mimes.contains(&MIME.to_string()),
        "B's data_offer advertised A's mime, got {:?}",
        b.offered_mimes
    );
    let offer = b.selection_offer.clone().unwrap();

    // ---- B pastes: receive over a real pipe; A's source writes the bytes ----
    let (mut reader, writer) = UnixStream::pair().expect("pipe pair");
    offer.receive(MIME.to_string(), writer.as_fd());
    let _ = qb.roundtrip(&mut b); // flush the receive + fd to the compositor
    drop(writer); // close our write end so EOF arrives once A's forwarded dup closes
    reader.set_nonblocking(true).expect("nonblocking reader");

    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = qa.roundtrip(&mut a); // let A process wl_data_source.send and write
        let _ = qb.roundtrip(&mut b);
        let mut tmp = [0u8; 256];
        match reader.read(&mut tmp) {
            Ok(0) => break, // EOF: A closed its end after writing
            Ok(n) => got.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("read from clipboard pipe failed: {e}"),
        }
        if Instant::now() >= deadline {
            panic!(
                "clipboard bytes never arrived; got {} bytes so far: {:?}",
                got.len(),
                got
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(got, PAYLOAD, "B pasted EXACTLY the bytes A copied");
    assert!(
        a.source_send_fired,
        "A's wl_data_source.send fired (the compositor routed B's receive to A)"
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
            if !app.drawn {
                app.surface.attach(Some(&app.buffer), 0, 0);
                app.surface.damage(0, 0, W, H);
                let _cb: WlCallback = app.surface.frame(qh, ());
                app.surface.commit();
                app.drawn = true;
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
            app.frame_done = true;
        }
    }
}
impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        e: <WlKeyboard as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter {
            serial, surface, ..
        } = e
        {
            if surface.id() == app.surface.id() {
                app.kbd_enter_serial = Some(serial);
            }
        }
    }
}
// ---- source side (A): honor the compositor's request to hand over the bytes ----
impl Dispatch<WlDataSource, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlDataSource,
        e: <WlDataSource as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_source::Event::Send { mime_type, fd } = e {
            assert_eq!(mime_type, MIME, "source asked for the mime it offered");
            // `fd` is an OwnedFd → write the payload, then drop (close) so the reader sees EOF.
            let mut f = std::fs::File::from(fd);
            f.write_all(PAYLOAD).expect("write clipboard payload");
            app.source_send_fired = true;
        }
    }
}
// ---- receiver side (B): collect offered mimes + the selected offer ----
impl Dispatch<WlDataDevice, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlDataDevice,
        e: <WlDataDevice as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_device::Event::Selection { id } = e {
            app.selection_offer = id;
        }
    }
    wayland_client::event_created_child!(App, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}
impl Dispatch<WlDataOffer, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlDataOffer,
        e: <WlDataOffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = e {
            app.offered_mimes.push(mime_type);
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
    WlDataDeviceManager
);
