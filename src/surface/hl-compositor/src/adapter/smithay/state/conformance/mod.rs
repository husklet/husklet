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
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

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
    /// Surface-local `wl_pointer.enter` coordinates, in order.
    pointer_enters: Vec<(f64, f64)>,
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
        if let xdg_toplevel::Event::WmCapabilities { capabilities } = event {
            app.capabilities = Some(
                capabilities
                    .chunks_exact(4)
                    .map(|word| u32::from_ne_bytes([word[0], word[1], word[2], word[3]]))
                    .collect(),
            );
        }
    }
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
            ..
        } = event
        {
            app.pointer_enters.push((surface_x, surface_y));
        }
    }
}

wayland_client::delegate_noop!(App: ignore WlRegion);
wayland_client::delegate_noop!(App: ignore WlCompositor);
wayland_client::delegate_noop!(App: ignore WlSurface);
wayland_client::delegate_noop!(App: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(App: ignore WlShmPool);
wayland_client::delegate_noop!(App: ignore WlBuffer);
wayland_client::delegate_noop!(App: ignore wp_viewporter::WpViewporter);
wayland_client::delegate_noop!(App: ignore wp_viewport::WpViewport);

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
        let path =
            std::env::temp_dir().join(format!("hl-conformance-{}-{w}x{h}.shm", std::process::id()));
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

mod shell;
mod surface;
