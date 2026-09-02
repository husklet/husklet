//! The socket one extension connects back on.
//!
//! An extension is given exactly one socket, and that socket is its
//! credential: whoever can open it holds the grant. The directory and the file
//! modes that make that true belong to [`SidecarSpec::prepare`], which is
//! called here rather than restated, and the listener's own job is only what
//! that cannot do — bind, accept, and hand the connection to a thread that
//! serves it.
//!
//! Shutdown is explicit rather than inherited from a dropped socket. The accept
//! loop polls a stop flag, and closing shuts the live connection's socket down
//! so the thread serving it returns from its blocking read, then joins it. A
//! listener that has been closed owns no running thread.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::SidecarSpec;

/// The one connection an extension may hold at a time, kept as a second
/// descriptor so shutdown can wake the thread serving it.
type Live = Arc<Mutex<Option<UnixStream>>>;

/// Why a listener could not be closed cleanly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// A served conversation had not ended when the deadline passed. Its thread
    /// is still joined, because a detached thread holding an extension's socket
    /// is worse than a slow shutdown; this says the wait ran long.
    Deadline(Duration),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Deadline(deadline) = self;
        write!(
            formatter,
            "an extension conversation was still running {} ms after it was asked to stop",
            deadline.as_millis()
        )
    }
}

impl std::error::Error for Fault {}

/// One extension's socket and the thread accepting on it.
pub struct Listener {
    socket: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    live: Live,
    ended: Option<mpsc::Receiver<()>>,
    accepting: Option<JoinHandle<()>>,
}

impl Listener {
    /// How often the accept loop rechecks the stop flag.
    ///
    /// `accept` cannot be interrupted, so the socket is non-blocking and the
    /// loop sleeps between tries. This is the latency of a shutdown, and the
    /// only thing paid for it is one wakeup per interval on an idle socket.
    const POLL: Duration = Duration::from_millis(20);

    /// How long [`Listener::close`] waits for a conversation to end.
    pub const DEADLINE: Duration = Duration::from_secs(5);

    /// Binds the extension's socket and starts accepting.
    ///
    /// `attend` is called on a thread of its own for each accepted connection,
    /// and owns the [`Conversation`](super::Conversation) it builds from the
    /// stream. Only one connection is served at a time: an extension has one
    /// conversation, so a second connection is closed immediately rather than
    /// racing the first for the same session.
    ///
    /// # Errors
    /// Returns the failure to prepare the socket directory, remove a stale
    /// socket, or bind.
    pub fn open<F>(spec: &SidecarSpec, attend: F) -> io::Result<Self>
    where
        F: Fn(UnixStream) + Send + Sync + 'static,
    {
        spec.prepare()?;
        let socket = spec.socket().to_path_buf();
        clear(&socket)?;
        let listener = UnixListener::bind(&socket)?;
        // Again after binding: `prepare` tightens a socket that is already
        // there, and the one bind just created was made under the process
        // umask.
        spec.prepare()?;
        listener.set_nonblocking(true)?;
        Ok(Self::start(socket, listener, Arc::new(attend)))
    }

    /// The socket extensions connect to.
    #[must_use]
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// Whether a conversation is being served right now.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.live.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Ends the accept loop, ends the conversation it is serving, and joins
    /// both threads.
    ///
    /// # Errors
    /// Returns `Fault::Deadline` when a conversation outlasted the deadline.
    /// The threads are joined either way, so no thread outlives this call.
    pub fn close(mut self) -> Result<(), Fault> {
        self.begin_shutdown().map_or(Ok(()), ListenerShutdown::wait)
    }

    fn start<F>(socket: std::path::PathBuf, listener: UnixListener, attend: Arc<F>) -> Self
    where
        F: Fn(UnixStream) + Send + Sync + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let live: Live = Arc::new(Mutex::new(None));
        let (sender, ended) = mpsc::channel();
        let accepting = std::thread::spawn({
            let stop = Arc::clone(&stop);
            let live = Arc::clone(&live);
            move || accept(&listener, &attend, &stop, &live, &sender)
        });
        Self {
            socket,
            stop,
            live,
            ended: Some(ended),
            accepting: Some(accepting),
        }
    }

    /// The whole of shutdown, written once so dropping a listener does exactly
    /// what closing it does.
    fn begin_shutdown(&mut self) -> Option<ListenerShutdown> {
        let Some(accepting) = self.accepting.take() else {
            return None;
        };
        self.stop.store(true, Ordering::Release);
        self.wake();
        Some(ListenerShutdown {
            socket: self.socket.clone(),
            ended: self.ended.take().expect("a live listener has its completion channel"),
            accepting,
        })
    }

    /// Shuts the live connection down so the thread reading it returns.
    ///
    /// Without this, a conversation blocked on a peer that never speaks again
    /// would keep the accept thread waiting on a join that cannot finish.
    fn wake(&self) {
        let held = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(stream) = held.as_ref() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

struct ListenerShutdown {
    socket: std::path::PathBuf,
    ended: mpsc::Receiver<()>,
    accepting: JoinHandle<()>,
}

impl ListenerShutdown {
    fn wait(self) -> Result<(), Fault> {
        let started = Instant::now();
        let late = self.ended.recv_timeout(Listener::DEADLINE).is_err() && started.elapsed() >= Listener::DEADLINE;
        let _ = self.accepting.join();
        let _ = std::fs::remove_file(&self.socket);
        if late {
            return Err(Fault::Deadline(Listener::DEADLINE));
        }
        Ok(())
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let Some(shutdown) = self.begin_shutdown() else { return };
        let socket = self.socket.clone();
        std::thread::spawn(move || {
            if let Err(fault) = shutdown.wait() {
                hl_log::hl_error!(hl_log::tag::RUNTIME, "extension socket {}: {fault}", socket.display());
            }
        });
    }
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Listener")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

/// Removes a socket left behind by a process that did not close it, the way
/// the workspace domain does before binding its own.
fn clear(socket: &std::path::Path) -> io::Result<()> {
    match std::fs::remove_file(socket) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Accepts until told to stop, then joins whatever it was serving.
///
/// The end signal is sent after that join, so a caller waiting on it learns
/// that every thread this loop started is finished, not merely that the loop is.
fn accept<F>(listener: &UnixListener, attend: &Arc<F>, stop: &AtomicBool, live: &Live, ended: &mpsc::Sender<()>)
where
    F: Fn(UnixStream) + Send + Sync + 'static,
{
    let mut serving = None;
    while !stop.load(Ordering::Acquire) {
        serving = step(listener, attend, live, serving);
    }
    if let Some(handle) = serving {
        let _ = handle.join();
    }
    let _ = ended.send(());
}

/// One turn of the accept loop.
fn step<F>(
    listener: &UnixListener,
    attend: &Arc<F>,
    live: &Live,
    serving: Option<JoinHandle<()>>,
) -> Option<JoinHandle<()>>
where
    F: Fn(UnixStream) + Send + Sync + 'static,
{
    let held = reap(serving);
    let Ok((stream, _)) = listener.accept() else {
        std::thread::sleep(Listener::POLL);
        return held;
    };
    if held.is_some() {
        // One extension, one conversation. A second caller is closed rather
        // than queued, so it learns immediately instead of waiting on a
        // session it will never be given.
        return held;
    }
    serve(stream, attend, live)
}

/// Joins a conversation that has already finished, so a long-lived listener
/// never accumulates handles.
fn reap(serving: Option<JoinHandle<()>>) -> Option<JoinHandle<()>> {
    let handle = serving?;
    if !handle.is_finished() {
        return Some(handle);
    }
    let _ = handle.join();
    None
}

/// Hands one connection to a thread of its own.
fn serve<F>(stream: UnixStream, attend: &Arc<F>, live: &Live) -> Option<JoinHandle<()>>
where
    F: Fn(UnixStream) + Send + Sync + 'static,
{
    let waker = stream.try_clone().ok()?;
    live.lock().unwrap_or_else(PoisonError::into_inner).replace(waker);
    let attend = Arc::clone(attend);
    let live = Arc::clone(live);
    Some(std::thread::spawn(move || {
        attend(stream);
        live.lock().unwrap_or_else(PoisonError::into_inner).take();
    }))
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};

    use hl_extension::{Capability, ExtensionName, Grant, Manifest, Resources};

    use super::super::Image;
    use super::{Listener, SidecarSpec};

    fn manifest() -> Manifest {
        Manifest {
            name: ExtensionName::new("sample").expect("name"),
            display_name: "Sample".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new([Capability::ContainerRead]),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: Resources::default(),
            filesystem_roots: Vec::new(),
        }
    }

    fn spec(socket: &std::path::Path) -> SidecarSpec {
        let manifest = manifest();
        SidecarSpec::new(
            &manifest,
            &manifest.capabilities,
            &Image {
                reference: "extension:1".to_owned(),
                digest: "sha256:aaaa".to_owned(),
                entrypoint: vec!["/usr/bin/extension".to_owned()],
                user: "1000:1000".to_owned(),
            },
            socket,
        )
    }

    /// Waits for a condition the accept thread reaches on its own schedule.
    fn until(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_bound_socket_is_owner_only_and_replaces_a_stale_one() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        std::fs::create_dir_all(socket.parent().expect("directory")).expect("directory");
        std::fs::write(&socket, b"left behind by a crash").expect("stale socket");

        let listener = Listener::open(&spec(&socket), |_| {}).expect("bound");

        let mode = std::fs::metadata(&socket).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the socket is the extension's credential");
        listener.close().expect("closed");
    }

    #[test]
    fn a_connection_is_served_on_a_thread_of_its_own() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let served = Arc::new(AtomicUsize::new(0));
        let listener = Listener::open(&spec(&socket), {
            let served = Arc::clone(&served);
            move |_stream| {
                served.fetch_add(1, Ordering::Release);
            }
        })
        .expect("bound");

        let peer = UnixStream::connect(&socket).expect("connected");

        assert!(
            until(|| served.load(Ordering::Acquire) == 1),
            "the connection is served"
        );
        drop(peer);
        listener.close().expect("closed");
    }

    #[test]
    fn a_second_caller_does_not_take_the_conversation_from_the_first() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let entered = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Barrier::new(2));
        let listener = Listener::open(&spec(&socket), {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            move |_stream| {
                entered.fetch_add(1, Ordering::Release);
                release.wait();
            }
        })
        .expect("bound");
        let first = UnixStream::connect(&socket).expect("connected");
        assert!(until(|| entered.load(Ordering::Acquire) == 1));

        let second = UnixStream::connect(&socket).expect("connected");

        assert!(!until(|| entered.load(Ordering::Acquire) > 1), "one at a time");
        drop((first, second));
        release.wait();
        listener.close().expect("closed");
    }

    #[test]
    fn closing_ends_the_accept_loop_and_leaves_no_thread_behind() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        // Held by every thread the listener starts, so the reference count is
        // the number of threads that outlived the close.
        let token = Arc::new(());
        let listener = Listener::open(&spec(&socket), {
            let token = Arc::clone(&token);
            move |stream| {
                let held = Arc::clone(&token);
                // Blocks until shutdown wakes it, which is what closing has to
                // survive.
                let _ = std::io::Read::read(&mut &stream, &mut [0_u8; 1]);
                drop(held);
            }
        })
        .expect("bound");
        let peer = UnixStream::connect(&socket).expect("connected");
        assert!(until(|| listener.is_busy()), "the conversation is under way");

        listener.close().expect("closed inside the deadline");

        assert_eq!(
            Arc::strong_count(&token),
            1,
            "every thread the listener started is joined"
        );
        assert!(!socket.exists(), "the socket is not left behind");
        assert!(UnixStream::connect(&socket).is_err(), "the accept loop has stopped");
        drop(peer);
    }

    #[test]
    fn dropping_a_listener_shuts_it_down_as_closing_does() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let token = Arc::new(());
        let listener = Listener::open(&spec(&socket), {
            let token = Arc::clone(&token);
            move |_stream| {
                drop(Arc::clone(&token));
            }
        })
        .expect("bound");

        drop(listener);

        assert!(
            until(|| Arc::strong_count(&token) == 1 && !socket.exists()),
            "the background reaper joins every thread and removes the socket"
        );
    }

    #[test]
    fn dropping_never_waits_for_a_stuck_conversation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let socket = temporary.path().join("run/extension.sock");
        let (release, blocked) = mpsc::channel();
        let blocked = Arc::new(Mutex::new(blocked));
        let (entered, serving) = mpsc::channel();
        let listener = Listener::open(&spec(&socket), {
            let blocked = Arc::clone(&blocked);
            move |_stream| {
                let _ = entered.send(());
                let _ = blocked.lock().expect("release").recv();
            }
        })
        .expect("bound");
        let peer = UnixStream::connect(&socket).expect("connected");
        serving
            .recv_timeout(Duration::from_secs(1))
            .expect("conversation entered");

        let started = Instant::now();
        drop(listener);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "drop must not join conversation work"
        );
        assert!(socket.exists(), "cleanup is genuinely still pending when drop returns");

        release.send(()).expect("release conversation");
        assert!(until(|| !socket.exists()), "the reaper still completes cleanup");
        drop(peer);
    }
}
