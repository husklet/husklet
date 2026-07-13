//! Client-roundtrip proof for `zwp_linux_explicit_synchronization_v1` (ledger row
//! `compositor_explicit_sync_waits_acquire_before_sampling_and_releases_after_gpu_completion`).
//!
//! A minimal in-process Wayland client connects over a `socketpair`, creates a surface + a
//! single-pixel buffer, and drives the real explicit-sync handshake: it hands the compositor an
//! **acquire fence** (an eventfd standing in for a `dma_fence` sync_file) and a **release** object,
//! then commits the buffer. We then prove the compositor's fence CONTRACT end to end:
//!
//!   * the acquire fence committed with the buffer is available to the present path BEFORE sampling
//!     ([`DdState::take_acquire_fence`]) and a real pollable-fd wait blocks until it signals
//!     ([`wait_acquire_fence`]);
//!   * signalling the release AFTER GPU completion reaches the client both as `immediate_release`
//!     (no fence) and as `fenced_release` carrying the compositor's completion fence fd;
//!   * committing an acquire/release without a buffer is a `no_buffer` protocol error.
//!
//! The Metal side (bridging the fence into an `MTLSharedEvent` wait/signal on the real host) lives in
//! `dd_display::explicit_sync_bridge` and is mac-gated; this Linux test exercises the protocol + the
//! CPU pollable-fd wait. Runs headlessly on Linux (libxkbcommon present) and macOS.

use dd_compositor::handlers::explicit_sync::wait_acquire_fence;
use dd_compositor::{ClientState, DdState};
use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{BorrowedFd, FromRawFd, RawFd};
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;
const WL_REGISTRY: u32 = 2;

struct CountingPresenter {
    frames: u32,
}
impl Presenter for CountingPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.frames += 1;
        Ok(PresentOutcome::Delivered { serial: self.frames as u64, timing: None })
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
}

struct Cli {
    conn: Conn,
    next_id: u32,
    globals: HashMap<String, (u32, u32)>,
    events: Vec<(u32, u16)>,
}
impl Cli {
    fn new(fd: RawFd) -> Cli {
        Cli { conn: Conn::new(fd), next_id: 2, globals: HashMap::new(), events: Vec::new() }
    }
    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    fn flush(&mut self) {
        self.conn.flush().unwrap();
    }
    fn drain(&mut self) {
        loop {
            match self.conn.fill().unwrap() {
                0 | -1 => break,
                _ => {}
            }
        }
        while let Some(m) = self.conn.next_message() {
            self.events.push((m.object, m.opcode));
            if m.opcode == 0 && m.object == WL_REGISTRY {
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            }
        }
    }
    fn saw(&self, object: u32, opcode: u16) -> bool {
        self.events.contains(&(object, opcode))
    }
    fn had_protocol_error(&self) -> bool {
        self.saw(WL_DISPLAY, 0)
    }
}

fn socketpair_nonblocking() -> (RawFd, RawFd) {
    let mut sv = [0i32; 2];
    assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
    for fd in sv {
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    (sv[0], sv[1])
}

fn unsignalled_eventfd() -> RawFd {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0, "eventfd");
    fd
}

fn signal_eventfd(fd: RawFd) {
    let v: u64 = 1;
    let n = unsafe { libc::write(fd, &v as *const u64 as *const libc::c_void, 8) };
    assert_eq!(n, 8, "signal eventfd");
}

#[test]
fn explicit_sync_acquire_release_fence_contract() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(CountingPresenter { frames: 0 }));

    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();
    let mut c = Cli::new(client_fd);

    macro_rules! pump {
        () => {{
            c.flush();
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }};
    }

    // Registry.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();
    for iface in ["zwp_linux_explicit_synchronization_v1", "wl_compositor", "wp_single_pixel_buffer_manager_v1"] {
        assert!(c.globals.contains_key(iface), "global {iface} not advertised; got {:?}", c.globals.keys().collect::<Vec<_>>());
    }
    let bind = |c: &mut Cli, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn.send(&Message::new(WL_REGISTRY, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };

    let comp = bind(&mut c, "wl_compositor", 4);
    let spb = bind(&mut c, "wp_single_pixel_buffer_manager_v1", 1);
    let sync_mgr = bind(&mut c, "zwp_linux_explicit_synchronization_v1", 2);

    // Surface 1 (compositor sid = 1) with a single-pixel buffer.
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // wl_compositor.create_surface
    let buffer = c.alloc();
    c.conn.send(&Message::new(spb, 1).u32(buffer).u32(0).u32(0).u32(0).u32(0xffff_ffff)); // create_u32_rgba_buffer
    // get_synchronization(id, surface) — manager opcode 1.
    let sync = c.alloc();
    c.conn.send(&Message::new(sync_mgr, 1).u32(sync).u32(surface));
    pump!();

    // set_acquire_fence(fd) — surface_synchronization opcode 1, fd rides SCM_RIGHTS.
    let acquire = unsignalled_eventfd();
    c.conn.queue_fd(acquire);
    c.conn.send(&Message::new(sync, 1));
    // get_release(id) — opcode 2.
    let release = c.alloc();
    c.conn.send(&Message::new(sync, 2).u32(release));
    // Attach the buffer and commit — the acquire fence + release now bind to this buffer.
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // wl_surface.attach
    c.conn.send(&Message::new(surface, 6)); // wl_surface.commit
    pump!();
    assert!(!c.had_protocol_error(), "valid acquire+release+buffer commit must not error; saw {:?}", c.events);

    // The commit bound the fences to surface sid 1.
    let sids = state.explicit_sync_committed_sids();
    assert_eq!(sids, vec![1], "the committed surface's explicit-sync fences must be tracked");
    let sid = sids[0];
    assert!(state.has_pending_acquire(sid), "acquire fence must be resident until sampled");
    assert!(state.has_committed_release(sid), "release must be owed until GPU completion");

    // Contract part 1: the present path takes the acquire fence BEFORE sampling, and a real fd wait
    // blocks until it signals.
    let taken = state.take_acquire_fence(sid).expect("acquire fence available before sampling");
    assert!(!state.has_pending_acquire(sid), "acquire fence consumed exactly once");
    let taken_fd = unsafe { BorrowedFd::borrow_raw(std::os::unix::io::AsRawFd::as_raw_fd(&taken)) };
    assert!(!wait_acquire_fence(taken_fd, 0).unwrap(), "unsignalled acquire fence must not be ready");
    signal_eventfd(acquire); // the client's GPU finished producing the buffer
    assert!(wait_acquire_fence(taken_fd, 200).unwrap(), "acquire wait must return once the fence signals");

    // Contract part 2a: signalling release with no fence reaches the client as immediate_release (op 1).
    assert!(state.signal_buffer_release(sid, None), "a release was owed and must be signalled");
    assert!(!state.has_committed_release(sid), "release is consumed once signalled");
    pump!();
    assert!(c.saw(release, 1), "client must receive zwp_linux_buffer_release_v1.immediate_release; saw {:?}", c.events);

    // Second commit → prove fenced_release carries the compositor's completion fence.
    let acquire2 = unsignalled_eventfd();
    c.conn.queue_fd(acquire2);
    c.conn.send(&Message::new(sync, 1)); // set_acquire_fence
    let release2 = c.alloc();
    c.conn.send(&Message::new(sync, 2).u32(release2)); // get_release
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(surface, 6)); // commit
    pump!();
    assert_eq!(state.explicit_sync_committed_sids(), vec![1]);
    let _ = state.take_acquire_fence(sid);
    let completion = unsignalled_eventfd(); // the compositor's Metal-completion fence
    signal_eventfd(completion);
    assert!(state.signal_buffer_release(sid, Some(unsafe { std::os::unix::io::OwnedFd::from_raw_fd(completion) })));
    pump!();
    assert!(c.saw(release2, 0), "client must receive fenced_release (opcode 0); saw {:?}", c.events);
    assert!(c.conn.take_fd().is_some(), "fenced_release must carry the compositor's completion fence fd");

    // Contract part 3: an acquire/release commit WITHOUT a buffer is a no_buffer protocol error. This
    // kills the client, so it is the last action.
    let surface2 = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface2)); // create_surface (sid 2)
    let sync2 = c.alloc();
    c.conn.send(&Message::new(sync_mgr, 1).u32(sync2).u32(surface2)); // get_synchronization
    pump!();
    let acquire3 = unsignalled_eventfd();
    c.conn.queue_fd(acquire3);
    c.conn.send(&Message::new(sync2, 1)); // set_acquire_fence
    let release3 = c.alloc();
    c.conn.send(&Message::new(sync2, 2).u32(release3)); // get_release
    c.conn.send(&Message::new(surface2, 6)); // commit WITHOUT attaching a buffer
    pump!();
    assert!(c.had_protocol_error(), "acquire/release without a buffer must raise a protocol error; saw {:?}", c.events);
}
