//! Wire-level protocol conformance: a real `wayland-client` driven against [`HlState`] over a socket
//! pair, pumped synchronously so every assertion reads exactly the bytes one dispatch round produced.
//!
//! These tests assert the CLIENT-observable contract — the events a toolkit reads and the protocol errors
//! a violation must raise — not compositor internals.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_keyboard, wl_pointer, wl_region::WlRegion,
    wl_registry, wl_seat, wl_shm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};

use super::{ClientState, HlState};
use crate::adapter::smithay::present::PngPresenter;

/// What the client recorded off the wire.
#[derive(Default)]
struct App {
    globals: Vec<(u32, String, u32)>,
    /// `xdg_toplevel.wm_capabilities`, decoded from the wire array of enum values.
    capabilities: Option<Vec<u32>>,
    configures: Vec<u32>,
    /// `wl_seat.capabilities` — whether the seat claims a keyboard at all.
    seat_capabilities: Option<u32>,
    /// The `wl_keyboard.keymap` format and size the client was handed.
    keymap: Option<(u32, u32)>,
    /// Surfaces named by `wl_keyboard.enter`, in order.
    keyboard_enters: Vec<u32>,
    /// Surfaces named by `wl_keyboard.leave`, in order — the other half of a focus transfer.
    keyboard_leaves: Vec<u32>,
    /// `wl_keyboard.key(keycode, state)` in order; the keycode is EVDEV, as the protocol specifies.
    keys: Vec<(u32, u32)>,
    /// Surface-local `wl_pointer.enter` coordinates, in order.
    pointer_enters: Vec<(f64, f64)>,
    /// The serial of the most recent `wl_pointer.enter` — the token `wl_pointer.set_cursor` and
    /// `wp_cursor_shape_device_v1.set_shape` must present for the compositor to honour them.
    last_enter_serial: Option<u32>,
    /// Every `wl_pointer` event in wire order — the ORDER is the contract: axis values and their source
    /// must arrive inside one `frame`, and an `axis_stop` terminates the sequence.
    pointer_events: Vec<PointerWire>,
    /// `xdg_toplevel.configure(width, height, states)` in order, states decoded from the wire array.
    toplevel_configures: Vec<(i32, i32, Vec<u32>)>,
    /// The serial of the most recent `wl_pointer.button` — what a client quotes back on a request that
    /// must be anchored to an input event (`xdg_toplevel.resize`, `move`, `xdg_popup.grab`).
    last_button_serial: Option<u32>,
    /// Every `xdg_popup` event, in wire order — the order matters: `repositioned` must precede the
    /// configure that describes the new geometry.
    popup_events: Vec<PopupWire>,
}

/// A `wl_pointer` event as the client read it off the wire.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PointerWire {
    Enter,
    Leave,
    Motion(f64, f64),
    /// `wl_pointer.button(button, state)`.
    Button(u32, u32),
    /// `wl_pointer.axis(axis, value)` — a smooth scroll amount.
    Axis(u32, f64),
    /// `wl_pointer.axis_source(source)`.
    Source(u32),
    /// `wl_pointer.axis_value120(axis, steps)` — high-resolution discrete detents (v8+).
    Value120(u32, i32),
    /// `wl_pointer.axis_stop(axis)` — the scroll sequence on that axis ended.
    Stop(u32),
    /// `wl_pointer.frame` — everything since the previous frame is one atomic update.
    Frame,
}

/// An `xdg_popup` event as the client read it off the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PopupWire {
    /// `xdg_popup.configure(x, y, width, height)` — the placement, relative to the parent's window
    /// geometry, the compositor resolved from the positioner.
    Configure(i32, i32, i32, i32),
    /// `xdg_popup.repositioned(token)` — the acknowledgement of an `xdg_popup.reposition`.
    Repositioned(u32),
    /// `xdg_popup.popup_done` — the compositor dismissed the popup.
    Done,
}

impl App {
    /// The most recent `xdg_popup.configure` geometry.
    fn popup_geometry(&self) -> Option<(i32, i32, i32, i32)> {
        self.popup_events
            .iter()
            .rev()
            .find_map(|event| match event {
                PopupWire::Configure(x, y, w, h) => Some((*x, *y, *w, *h)),
                _ => None,
            })
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut App,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            app.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _app: &mut App,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut App,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            app.configures.push(serial);
            xdg.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut App,
        _toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        match event {
            xdg_toplevel::Event::WmCapabilities { capabilities } => {
                app.capabilities = Some(decode_enum_array(&capabilities));
            }
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                app.toplevel_configures
                    .push((width, height, decode_enum_array(&states)));
            }
            _ => {}
        }
    }
}

/// Decode a wire array of 32-bit protocol enum values (`wm_capabilities`, `xdg_toplevel.configure` states).
fn decode_enum_array(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|word| u32::from_ne_bytes([word[0], word[1], word[2], word[3]]))
        .collect()
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        app: &mut App,
        _seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            app.seat_capabilities = Some(capabilities.into());
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut App,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, size, .. } => {
                app.keymap = Some((format.into(), size));
            }
            wl_keyboard::Event::Enter { surface, .. } => {
                app.keyboard_enters.push(surface.id().protocol_id());
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                app.keyboard_leaves.push(surface.id().protocol_id());
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                app.keys.push((key, state.into()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        app: &mut App,
        _pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let wl_pointer::Event::Enter {
            surface_x,
            surface_y,
            serial,
            ..
        } = event
        {
            app.pointer_enters.push((surface_x, surface_y));
            app.last_enter_serial = Some(serial);
        }
        app.pointer_events.push(match event {
            wl_pointer::Event::Enter { .. } => PointerWire::Enter,
            wl_pointer::Event::Leave { .. } => PointerWire::Leave,
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => PointerWire::Motion(surface_x, surface_y),
            wl_pointer::Event::Button {
                button,
                state,
                serial,
                ..
            } => {
                app.last_button_serial = Some(serial);
                PointerWire::Button(button, state.into())
            }
            wl_pointer::Event::Axis { axis, value, .. } => PointerWire::Axis(axis.into(), value),
            wl_pointer::Event::AxisSource { axis_source } => {
                PointerWire::Source(axis_source.into())
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                PointerWire::Value120(axis.into(), value120)
            }
            wl_pointer::Event::AxisStop { axis, .. } => PointerWire::Stop(axis.into()),
            wl_pointer::Event::Frame => PointerWire::Frame,
            _ => return,
        });
    }
}

impl Dispatch<xdg_popup::XdgPopup, ()> for App {
    fn event(
        app: &mut App,
        _popup: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        app.popup_events.push(match event {
            xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => PopupWire::Configure(x, y, width, height),
            xdg_popup::Event::Repositioned { token } => PopupWire::Repositioned(token),
            xdg_popup::Event::PopupDone => PopupWire::Done,
            _ => return,
        });
    }
}

wayland_client::delegate_noop!(App: ignore xdg_positioner::XdgPositioner);
wayland_client::delegate_noop!(App: ignore WlRegion);
wayland_client::delegate_noop!(App: ignore WlCompositor);
wayland_client::delegate_noop!(App: ignore WlSurface);
wayland_client::delegate_noop!(App: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(App: ignore WlShmPool);
wayland_client::delegate_noop!(App: ignore WlBuffer);
wayland_client::delegate_noop!(App: ignore wp_viewporter::WpViewporter);
wayland_client::delegate_noop!(App: ignore wp_viewport::WpViewport);
wayland_client::delegate_noop!(App: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
wayland_client::delegate_noop!(App: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);

/// A compositor and one connected client, driven on this thread with no sockets, threads or env.
struct Fixture {
    display: Display<HlState>,
    state: HlState,
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
}

impl Fixture {
    fn new() -> Fixture {
        let display: Display<HlState> = Display::new().expect("wayland display");
        let state = HlState::new(&display.handle(), PngPresenter::new());
        let (server, client) = UnixStream::pair().expect("socket pair");
        client.set_nonblocking(true).expect("nonblocking client");
        display
            .handle()
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("insert client");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue: EventQueue<App> = conn.new_event_queue();
        let qh = queue.handle();
        let mut fixture = Fixture {
            display,
            state,
            conn,
            queue,
            qh,
            app: App::default(),
        };
        let _registry = fixture.conn.display().get_registry(&fixture.qh, ());
        fixture.pump();
        fixture
    }

    /// Exchange requests and events until both sides are quiet.
    fn pump(&mut self) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.app);
        }
    }

    /// Bind an advertised global at `version`, panicking when it is not advertised at least that high.
    fn bind<I>(&mut self, version: u32) -> I
    where
        I: Proxy + 'static,
        App: Dispatch<I, ()>,
    {
        let wanted = I::interface().name;
        let (name, advertised) = self
            .app
            .globals
            .iter()
            .find(|(_, interface, _)| interface == wanted)
            .map(|(name, _, advertised)| (*name, *advertised))
            .unwrap_or_else(|| panic!("{wanted} is not advertised"));
        assert!(
            advertised >= version,
            "{wanted} advertised at v{advertised}, below the required v{version}"
        );
        let registry = self.conn.display().get_registry(&self.qh, ());
        registry.bind(name, version, &self.qh, ())
    }

    /// A `w`×`h` ARGB8888 `wl_shm` buffer over an unlinked backing file.
    fn buffer(&mut self, shm: &wl_shm::WlShm, w: i32, h: i32) -> WlBuffer {
        use std::io::Write;
        use std::os::fd::AsFd;

        let stride = w * 4;
        let size = (stride * h) as usize;
        // Unique per call: these tests run concurrently in one process, so a path keyed only by pid+size
        // lets two fixtures truncate each other's backing file and the compositor's mmap then fails.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hl-conformance-{}-{nonce}-{w}x{h}.shm",
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("shm backing file");
        file.write_all(&vec![0x40u8; size]).expect("shm pixels");
        file.flush().expect("flush shm pixels");
        let _ = std::fs::remove_file(&path);
        let pool = shm.create_pool(file.as_fd(), size as i32, &self.qh, ());
        std::mem::forget(file);
        pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, &self.qh, ())
    }

    fn protocol_error(&self) -> Option<wayland_client::backend::protocol::ProtocolError> {
        self.conn.protocol_error()
    }
}

mod cursor;
mod focus;
mod input;
mod popup;
mod resize;
mod shell;
mod surface;
