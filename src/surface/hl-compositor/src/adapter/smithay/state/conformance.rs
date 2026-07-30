//! Wire-level protocol conformance: a real `wayland-client` driven against [`HlState`] over a socket
//! pair, pumped synchronously so every assertion reads exactly the bytes one dispatch round produced.
//!
//! These tests assert the CLIENT-observable contract — the events a toolkit reads and the protocol errors
//! a violation must raise — not compositor internals.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
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

#[test]
fn a_toplevel_learns_the_window_management_operations_the_compositor_performs() {
    // GTK4/Qt/Chrome read `wm_capabilities` to decide which window controls to offer. Claiming an
    // operation the compositor drops (`window_menu`) gives the user a control that does nothing.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
    let surface = compositor.create_surface(&fixture.qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
    let _toplevel = xdg.get_toplevel(&fixture.qh, ());
    surface.commit();
    fixture.pump();

    assert!(
        !fixture.app.configures.is_empty(),
        "the toplevel never received an xdg_surface.configure"
    );
    let capabilities = fixture
        .app
        .capabilities
        .clone()
        .expect("xdg_toplevel.wm_capabilities was never sent");
    for honoured in [
        xdg_toplevel::WmCapabilities::Maximize,
        xdg_toplevel::WmCapabilities::Minimize,
        xdg_toplevel::WmCapabilities::Fullscreen,
    ] {
        assert!(
            capabilities.contains(&(honoured as u32)),
            "{honoured:?} is honoured but was not advertised: {capabilities:?}"
        );
    }
    assert!(
        !capabilities.contains(&(xdg_toplevel::WmCapabilities::WindowMenu as u32)),
        "window_menu is a no-op and must not be advertised: {capabilities:?}"
    );
}

#[test]
fn a_client_provided_cursor_surface_is_never_presented_as_a_window() {
    // `wl_pointer.set_cursor` hands the compositor a surface to draw as the cursor. Left roleless it
    // becomes its own window root: every cursor update composes a stray frame, and because a roleless
    // surface has no window to arm a repaint against, its frame callbacks are never released — an
    // animated cursor stalls.
    use smithay::input::pointer::CursorImageStatus;
    use smithay::input::SeatHandler;

    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let _cursor = compositor.create_surface(&fixture.qh, ());
    fixture.pump();

    let sid = crate::scene::model::SurfaceId(1);
    let wl = fixture
        .state
        .surfaces_by_id
        .get(&sid)
        .cloned()
        .expect("the client's surface reached the scene");
    let seat = fixture.state.seat.clone();
    fixture
        .state
        .cursor_image(&seat, CursorImageStatus::Surface(wl));

    assert_eq!(
        fixture.state.engine.scene.get(sid).map(|s| s.role.clone()),
        Some(crate::scene::model::SurfaceRole::Cursor)
    );
    assert!(
        fixture.state.engine.complete_commit(sid, true).frame.is_none(),
        "a cursor surface must not drive a window present"
    );
}

#[test]
fn a_viewport_source_outside_the_buffer_raises_out_of_buffer() {
    // Unenforced, the presenter clamps the sample to the buffer edge and the client silently renders
    // smeared pixels. The protocol requires the error on the wp_viewport object instead.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let viewporter: wp_viewporter::WpViewporter = fixture.bind(1);
    let surface = compositor.create_surface(&fixture.qh, ());
    let viewport = viewporter.get_viewport(&surface, &fixture.qh, ());
    let buffer = fixture.buffer(&shm, 4, 4);

    viewport.set_source(0.0, 0.0, 8.0, 8.0);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 4, 4);
    surface.commit();
    fixture.pump();

    let error = fixture
        .protocol_error()
        .expect("an out-of-buffer source crop must raise a protocol error");
    assert_eq!(error.object_interface, "wp_viewport");
    assert_eq!(error.code, wp_viewport::Error::OutOfBuffer as u32);
}

#[test]
fn a_viewport_source_inside_the_buffer_is_honoured() {
    // The control for the check above: a legal crop+scale must survive untouched, and the scene must
    // resolve the destination size the client asked for.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let viewporter: wp_viewporter::WpViewporter = fixture.bind(1);
    let surface = compositor.create_surface(&fixture.qh, ());
    let viewport = viewporter.get_viewport(&surface, &fixture.qh, ());
    let buffer = fixture.buffer(&shm, 8, 8);

    viewport.set_source(2.0, 2.0, 4.0, 4.0);
    viewport.set_destination(16, 16);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 8, 8);
    surface.commit();
    fixture.pump();

    assert!(fixture.protocol_error().is_none());
    let sid = crate::scene::model::SurfaceId(1);
    let logical = fixture
        .state
        .engine
        .scene
        .get(sid)
        .and_then(|surface| surface.logical_size());
    assert_eq!(logical, Some((16, 16)));
}
