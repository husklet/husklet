//! The Wayland socket serve loop — the neutral analogue of `hl-compositor`'s `main.rs::build_loop`.
//!
//! Binds a listening Unix socket, drives it and the `wl_display` fd through a `calloop` event loop, and
//! dispatches client requests into [`HlState`]'s handlers. Everything Wayland here is `!Send` (the
//! `Display` holds `Rc`s), so the whole loop — `Display`, `HlState`, and the `PngPresenter` — is created
//! and run on ONE thread; a caller that wants it off-thread spawns a thread whose body calls [`run`].

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use smithay::reexports::{
    calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction},
    wayland_server::Display,
};

use super::present::PngPresenter;
use super::state::{ClientState, HlState};

struct LoopData {
    state: HlState,
    display: Display<HlState>,
}

/// Bind `socket_path`, stand up the compositor, and dispatch until `stop` is set.
///
/// Creates the `Display` + [`HlState`] + engine on the calling thread (all `!Send`). `presenter` and
/// `stop` cross the thread boundary from the caller (both `Send`); grab the presenter's captures handle
/// BEFORE calling this.
pub fn run(socket_path: &Path, presenter: PngPresenter, stop: Arc<AtomicBool>) -> std::io::Result<()> {
    let mut display: Display<HlState> = Display::new().expect("create wl_display");
    let dh = display.handle();
    let state = HlState::new(&dh, presenter);

    let _ = std::fs::remove_file(socket_path); // clear a stale socket
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;

    let event_loop: EventLoop<LoopData> = EventLoop::try_new().expect("calloop event loop");
    let handle = event_loop.handle();

    // Accept new clients: each connection becomes a wayland-server client with its own ClientState.
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            // calloop's `Generic` hands back `&mut NoIoDrop<UnixListener>`; it derefs to the listener.
            |_, listener, data: &mut LoopData| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(true);
                            let cs: Arc<ClientState> = Arc::new(data.state.new_client_state());
                            if let Err(e) = data.display.handle().insert_client(stream, cs) {
                                eprintln!("hl_wip-compositor: insert_client failed: {e}");
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            eprintln!("hl_wip-compositor: accept error: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("insert socket source");

    // Drive the wl_display fd so queued client requests dispatch into the handlers.
    let display_fd = display.backend().poll_fd().try_clone_to_owned().expect("dup display fd");
    handle
        .insert_source(
            Generic::new(display_fd, Interest::READ, Mode::Level),
            |_, _, data: &mut LoopData| {
                if let Err(e) = data.display.dispatch_clients(&mut data.state) {
                    eprintln!("hl_wip-compositor: dispatch_clients error (continuing): {e}");
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("insert display source");

    let mut event_loop = event_loop;
    let mut data = LoopData { state, display };
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(16)), &mut data) {
            eprintln!("hl_wip-compositor: event loop dispatch error (continuing): {e}");
        }
        let _ = data.display.flush_clients();
    }
    Ok(())
}
