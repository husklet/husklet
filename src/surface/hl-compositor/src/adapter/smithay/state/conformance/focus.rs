//! Keyboard focus transfer between two SEPARATE clients: what each one observes on the wire.
//!
//! One client cannot prove focus transfer — the interesting half is that the client LOSING focus is told
//! (`wl_keyboard.leave`) and stops receiving keys, and that the client gaining it is told
//! (`wl_keyboard.enter`) and receives them. A toolkit that never sees the `leave` keeps drawing itself
//! active and keeps its own key handlers armed.

use std::os::fd::AsFd;

use super::*;

/// A SECOND client on the same compositor, with its own connection, queue and recorded observations.
struct Peer {
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
}

impl Peer {
    /// Connect another client to `fixture`'s display over its own socket pair.
    fn join(fixture: &mut Fixture) -> Peer {
        let (server, client) = UnixStream::pair().expect("socket pair");
        client.set_nonblocking(true).expect("nonblocking client");
        fixture
            .display
            .handle()
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("insert peer client");
        let conn = Connection::from_socket(client).expect("peer connection");
        let queue: EventQueue<App> = conn.new_event_queue();
        let qh = queue.handle();
        let peer = Peer {
            conn,
            queue,
            qh,
            app: App::default(),
        };
        let _registry = peer.conn.display().get_registry(&peer.qh, ());
        peer
    }

    /// Bind an advertised global at `version` from the peer's own registry.
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
            .unwrap_or_else(|| panic!("{wanted} is not advertised to the peer"));
        assert!(
            advertised >= version,
            "{wanted} advertised at v{advertised}"
        );
        let registry = self.conn.display().get_registry(&self.qh, ());
        registry.bind(name, version, &self.qh, ())
    }
}

/// Exchange requests and events for BOTH clients until everyone is quiet.
fn pump_both(fixture: &mut Fixture, peer: &mut Peer) {
    for _ in 0..8 {
        let _ = fixture.conn.flush();
        let _ = peer.conn.flush();
        let _ = fixture.display.dispatch_clients(&mut fixture.state);
        let _ = fixture.display.flush_clients();
        if let Some(guard) = fixture.conn.prepare_read() {
            let _ = guard.read();
        }
        if let Some(guard) = peer.conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = fixture.queue.dispatch_pending(&mut fixture.app);
        let _ = peer.queue.dispatch_pending(&mut peer.app);
    }
}

#[test]
fn keyboard_focus_moves_between_two_clients_and_only_the_focused_one_receives_keys() {
    const KEY_A: u32 = 30; // evdev KEY_A
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let seat: wl_seat::WlSeat = fixture.bind(8);
    fixture.pump();
    let keyboard_a = seat.get_keyboard(&fixture.qh, ());
    let surface_a = compositor.create_surface(&fixture.qh, ());
    let xdg_a = wm_base.get_xdg_surface(&surface_a, &fixture.qh, ());
    let _toplevel_a = xdg_a.get_toplevel(&fixture.qh, ());
    surface_a.commit();
    fixture.pump();
    let buffer_a = fixture.buffer(&shm, 16, 16);
    surface_a.attach(Some(&buffer_a), 0, 0);
    surface_a.damage(0, 0, 16, 16);
    surface_a.commit();
    fixture.pump();

    let mut peer = Peer::join(&mut fixture);
    pump_both(&mut fixture, &mut peer);
    let peer_compositor: WlCompositor = peer.bind(4);
    let peer_wm_base: xdg_wm_base::XdgWmBase = peer.bind(5);
    let peer_shm: wl_shm::WlShm = peer.bind(1);
    let peer_seat: wl_seat::WlSeat = peer.bind(8);
    pump_both(&mut fixture, &mut peer);
    let keyboard_b = peer_seat.get_keyboard(&peer.qh, ());
    let surface_b = peer_compositor.create_surface(&peer.qh, ());
    let xdg_b = peer_wm_base.get_xdg_surface(&surface_b, &peer.qh, ());
    let _toplevel_b = xdg_b.get_toplevel(&peer.qh, ());
    surface_b.commit();
    pump_both(&mut fixture, &mut peer);
    let buffer_b = {
        let stride = 16 * 4;
        let size = stride * 16;
        let file = tempfile_of(size as usize);
        let pool = peer_shm.create_pool(file.as_fd(), size, &peer.qh, ());
        std::mem::forget(file);
        pool.create_buffer(0, 16, 16, stride, wl_shm::Format::Argb8888, &peer.qh, ())
    };
    surface_b.attach(Some(&buffer_b), 0, 0);
    surface_b.damage(0, 0, 16, 16);
    surface_b.commit();
    pump_both(&mut fixture, &mut peer);

    // Focus the second client explicitly, then look at BOTH sides of the transfer.
    fixture
        .state
        .apply_input(crate::adapter::smithay::InputCommand::FocusToplevelIndex(1));
    pump_both(&mut fixture, &mut peer);
    assert_eq!(
        peer.app.keyboard_enters,
        vec![surface_b.id().protocol_id()],
        "the newly focused client never received wl_keyboard.enter"
    );
    assert_eq!(
        fixture.app.keyboard_leaves,
        vec![surface_a.id().protocol_id()],
        "the client that lost focus was never sent wl_keyboard.leave"
    );

    // A key now belongs to exactly one client.
    fixture
        .state
        .apply_input(crate::adapter::smithay::InputCommand::Key {
            keycode: KEY_A,
            pressed: true,
        });
    pump_both(&mut fixture, &mut peer);
    assert_eq!(
        peer.app.keys,
        vec![(KEY_A, u32::from(wl_keyboard::KeyState::Pressed))],
        "the focused client did not receive the key as an evdev keycode"
    );
    assert!(
        fixture.app.keys.is_empty(),
        "an unfocused client received a key: {:?}",
        fixture.app.keys
    );

    // And transferring back reverses both halves.
    fixture
        .state
        .apply_input(crate::adapter::smithay::InputCommand::FocusToplevelIndex(0));
    pump_both(&mut fixture, &mut peer);
    assert_eq!(
        peer.app.keyboard_leaves,
        vec![surface_b.id().protocol_id()],
        "focus returned to the first client without telling the second it lost focus"
    );
    assert_eq!(
        fixture.app.keyboard_enters.len(),
        2,
        "the first client was not re-entered: {:?}",
        fixture.app.keyboard_enters
    );

    keyboard_a.release();
    keyboard_b.release();
}

/// An unlinked, zero-filled backing file of `size` bytes for a peer's `wl_shm` pool.
fn tempfile_of(size: usize) -> std::fs::File {
    use std::io::Write;
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "hl-conformance-peer-{}-{nonce}.shm",
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("peer shm backing file");
    file.write_all(&vec![0x60u8; size])
        .expect("peer shm pixels");
    file.flush().expect("flush peer shm pixels");
    let _ = std::fs::remove_file(&path);
    file
}
