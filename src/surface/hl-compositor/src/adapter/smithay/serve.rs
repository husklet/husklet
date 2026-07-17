//! The Wayland socket serve loop — the neutral analogue of `hl-compositor`'s `main.rs::build_loop`.
//!
//! Binds a listening Unix socket, drives it and the `wl_display` fd through a `calloop` event loop, and
//! dispatches client requests into [`HlState`]'s handlers. Everything Wayland here is `!Send` (the
//! `Display` holds `Rc`s), so the whole loop — `Display`, `HlState`, and the selected presenter — is created
//! and run on ONE thread; a caller that wants it off-thread spawns a thread whose body calls [`run`].

use std::ffi::OsString;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use smithay::reexports::{
    calloop::{
        channel::{Channel, Event as ChannelEvent},
        generic::Generic,
        EventLoop, Interest, Mode, PostAction,
    },
    wayland_server::Display,
};
use smithay::wayland::socket::ListeningSocketSource;

use hl_log::{hl_info, tag};

#[cfg(all(target_os = "macos", feature = "macos-surface"))]
fn with_platform_pool<T>(f: impl FnOnce() -> T) -> T {
    objc2::rc::autoreleasepool(|_| f())
}

#[cfg(not(all(target_os = "macos", feature = "macos-surface")))]
fn with_platform_pool<T>(f: impl FnOnce() -> T) -> T {
    f()
}

use super::present::AdapterPresenter;
use super::state::{ClientState, HlState, InputCommand};
use crate::scene::port::{Clock, Presenter, PresenterEvent};

/// The cross-thread sender half of an [`InputCommand`] channel. `Send` — a host/test on another thread
/// injects input through it while the serve loop runs.
pub use smithay::reexports::calloop::channel::{Channel as InputChannel, Sender as InputSender};

/// Create a host/test input channel: the [`InputSender`] stays with the caller (any thread), the
/// [`InputChannel`] is handed to [`run_auto_with_input`], which drains it into the seat. A convenience
/// so callers that do not depend on Smithay directly can still build the channel.
pub fn input_channel() -> (InputSender<InputCommand>, InputChannel<InputCommand>) {
    smithay::reexports::calloop::channel::channel()
}

struct LoopData {
    state: HlState,
    display: Display<HlState>,
}

impl LoopData {
    /// Turn an accepted client stream into a wayland-server client with its own [`ClientState`]. Shared by
    /// both socket paths (raw bind and the standard `ListeningSocketSource`).
    fn insert_client(&mut self, stream: UnixStream) {
        let _ = stream.set_nonblocking(true);
        let cs: Arc<ClientState> = Arc::new(self.state.new_client_state());
        if let Err(e) = self.display.handle().insert_client(stream, cs) {
            eprintln!("hl-compositor: insert_client failed: {e}");
            return;
        }
        hl_info!(tag::WAYLAND, "client connected");
    }
}

/// Bind `socket_path`, stand up the compositor, and dispatch until `stop` is set.
///
/// Creates the `Display` + [`HlState`] + engine on the calling thread (all `!Send`). `presenter` and
/// The caller chooses the presenter; the loop does not select a platform backend.
pub fn run(
    socket_path: &Path,
    presenter: impl Into<AdapterPresenter>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let display: Display<HlState> = Display::new().expect("create wl_display");
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
                        Ok((stream, _)) => data.insert_client(stream),
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            eprintln!("hl-compositor: accept error: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("insert socket source");

    drive(event_loop, LoopData { state, display }, stop)
}

/// Bind the STANDARD Wayland discovery socket and serve until `stop` — the path a real GUI toolkit takes.
///
/// Where [`run`] binds a bespoke absolute-path socket (handed to a client that already knows the path),
/// this uses Smithay's [`ListeningSocketSource`] to bind `$XDG_RUNTIME_DIR/wayland-N` with the sibling
/// `.lock` file, exactly as a real compositor does. A real client then finds it through `$WAYLAND_DISPLAY`
/// (Smithay picks the first free `wayland-1..=wayland-32`). The chosen socket name is handed to `on_bound`
/// BEFORE the serve loop starts — e.g. to publish `WAYLAND_DISPLAY` for a client about to connect.
///
/// `$XDG_RUNTIME_DIR` must be set (and absolute) in the environment before this is called; that is where
/// the socket is created.
pub fn run_auto(
    presenter: impl Into<AdapterPresenter>,
    stop: Arc<AtomicBool>,
    on_bound: impl FnOnce(OsString),
) -> std::io::Result<()> {
    run_auto_inner(presenter.into(), stop, None, on_bound)
}

/// Like [`run_auto`], but additionally drains a host/test-driven [`InputCommand`] channel each time a
/// command arrives, delivering pointer + keyboard input to the focused client through the seat.
///
/// This is the headless input seam: there is no hardware input source, so a caller (a test, or a host
/// that translates its own input) sends [`InputCommand`]s down `input` and the serve loop applies them
/// to [`HlState`] via [`HlState::apply_input`] — driving smithay's `PointerHandle`/`KeyboardHandle` so a
/// real Wayland client receives the wire events. The channel's [`smithay::reexports::calloop::channel::Sender`]
/// is `Send`, so a caller on another thread injects input while the loop runs here.
pub fn run_auto_with_input(
    presenter: impl Into<AdapterPresenter>,
    stop: Arc<AtomicBool>,
    input: Channel<InputCommand>,
    on_bound: impl FnOnce(OsString),
) -> std::io::Result<()> {
    run_auto_inner(presenter.into(), stop, Some(input), on_bound)
}

fn run_auto_inner(
    presenter: AdapterPresenter,
    stop: Arc<AtomicBool>,
    input: Option<Channel<InputCommand>>,
    on_bound: impl FnOnce(OsString),
) -> std::io::Result<()> {
    let display: Display<HlState> = Display::new().expect("create wl_display");
    let dh = display.handle();
    let state = HlState::new(&dh, presenter);

    // The real discovery socket: `$XDG_RUNTIME_DIR/wayland-N` + its `.lock`, the same seam a real client
    // reaches through `$WAYLAND_DISPLAY`.
    let source = ListeningSocketSource::new_auto().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("bind wayland socket: {e}"),
        )
    })?;
    let socket_name = source.socket_name().to_os_string();
    hl_info!(
        tag::WAYLAND,
        "socket bound name={}",
        socket_name.to_string_lossy()
    );
    on_bound(socket_name);

    let event_loop: EventLoop<LoopData> = EventLoop::try_new().expect("calloop event loop");
    let handle = event_loop.handle();
    handle
        .insert_source(source, |stream, _, data: &mut LoopData| {
            // `ListeningSocketSource` yields one already-accepted client stream per invocation.
            data.insert_client(stream);
        })
        .expect("insert listening socket source");

    // The host/test input seam: each `InputCommand` is applied to the seat as it arrives (a channel
    // message also wakes the loop, so injected input is delivered promptly, not a tick later).
    if let Some(input) = input {
        handle
            .insert_source(input, |event, _, data: &mut LoopData| {
                if let ChannelEvent::Msg(cmd) = event {
                    data.state.apply_input(cmd);
                }
            })
            .expect("insert input channel source");
    }

    drive(event_loop, LoopData { state, display }, stop)
}

/// The shared serve loop: insert the `wl_display` dispatch source, then dispatch + pace until `stop`. The
/// client-accept source is already inserted on `event_loop` by the caller ([`run`] or [`run_auto`]).
fn drive(
    event_loop: EventLoop<LoopData>,
    mut data: LoopData,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let handle = event_loop.handle();

    // Drive the wl_display fd so queued client requests dispatch into the handlers.
    let display_fd = data
        .display
        .backend()
        .poll_fd()
        .try_clone_to_owned()
        .expect("dup display fd");
    handle
        .insert_source(
            Generic::new(display_fd, Interest::READ, Mode::Level),
            |_, _, data: &mut LoopData| {
                if let Err(e) = data.display.dispatch_clients(&mut data.state) {
                    eprintln!("hl-compositor: dispatch_clients error (continuing): {e}");
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("insert display source");

    // Native AppKit events are drained by `Presenter::poll_events` below rather than through a calloop
    // source, so keep their worst-case input latency well below one ProMotion frame. Other targets retain
    // the low-power 60 Hz fallback. Repaint deadlines still shorten either wait precisely.
    #[cfg(target_os = "macos")]
    const TICK: Duration = Duration::from_millis(1);
    #[cfg(not(target_os = "macos"))]
    const TICK: Duration = Duration::from_millis(16);

    let mut event_loop = event_loop;
    while !stop.load(Ordering::Relaxed) {
        with_platform_pool(|| {
            let _tick = hl_log::hl_span!(tag::COMPOSITOR, "event_loop_tick");
            // Wake no later than the nearest owed repaint, so a throttled frame ships at its refresh boundary
            // instead of a whole tick late; otherwise sleep a full tick waiting on the socket.
            let now = data.state.engine.clock().now_nanos();
            let wait = match data.state.next_repaint_deadline() {
                Some(deadline) => Duration::from_nanos(deadline.saturating_sub(now)).min(TICK),
                None => TICK,
            };
            if let Err(e) = event_loop.dispatch(Some(wait), &mut data) {
                eprintln!("hl-compositor: event loop dispatch error (continuing): {e}");
            }
            data.state.engine.presenter_mut().poll_events();
            let host_events = data.state.engine.presenter_mut().take_events();
            for event in host_events {
                let command = match event {
                    PresenterEvent::PointerMotion { window, x, y } => {
                        InputCommand::PointerMotionOn { window, x, y }
                    }
                    PresenterEvent::PointerButton {
                        window,
                        button,
                        pressed,
                        click_count,
                    } => InputCommand::PointerButtonOn {
                        window,
                        button,
                        pressed,
                        click_count,
                    },
                    PresenterEvent::PointerAxis {
                        horizontal,
                        vertical,
                    } => InputCommand::PointerAxis {
                        horizontal,
                        vertical,
                    },
                    PresenterEvent::Key { keycode, pressed } => {
                        InputCommand::Key { keycode, pressed }
                    }
                    PresenterEvent::Resize {
                        surface,
                        width,
                        height,
                        maximized,
                        fullscreen,
                        resizing,
                    } => InputCommand::ResizeSurface {
                        surface,
                        width,
                        height,
                        maximized,
                        fullscreen,
                        resizing,
                    },
                    PresenterEvent::ResizeEnd { surface } => {
                        InputCommand::ResizeSurfaceEnd { surface }
                    }
                    PresenterEvent::Focus(surface) => InputCommand::FocusSurface(surface),
                    PresenterEvent::Close(surface) => InputCommand::CloseSurface(surface),
                };
                data.state.apply_input(command);
            }
            data.state.sync_clipboard();
            // Re-drive any frame the vsync throttle withheld whose refresh boundary has now arrived — this is
            // what actually ships a late commit + releases the client's frame callback when the client has
            // gone idle (a real commit arriving first supersedes it, so no double present). Then flush.
            data.state.drive_due_repaints();
            let _ = data.display.flush_clients();
        });
    }
    hl_log::flush();
    Ok(())
}
