use std::fmt;
use std::sync::{Arc, Condvar, Mutex};

use hl_gpu::{Capabilities, CommandSink, FeatureRequest, RemoteCommandSink};

use super::actor::Sequencer;

#[derive(Clone)]
pub(crate) struct Ready {
    pub(crate) capabilities: Capabilities,
    pub(crate) request: FeatureRequest,
    pub(crate) sequencer: Sequencer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InitializeError(Arc<str>);

impl fmt::Display for InitializeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InitializeError {}

enum Initialization {
    Uninitialized(Option<RemoteCommandSink>),
    Initializing,
    Ready(Ready),
    Failed(InitializeError),
}

/// One display's eagerly configured transport. Its sink is consumed exactly once.
pub(crate) struct DisplayTransport {
    state: Mutex<Initialization>,
    changed: Condvar,
}

impl DisplayTransport {
    pub(crate) fn new(sink: RemoteCommandSink) -> Self {
        Self {
            state: Mutex::new(Initialization::Uninitialized(Some(sink))),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn ensure(&self, request: &FeatureRequest) -> Result<Ready, InitializeError> {
        self.ensure_with(request, |sink, request| sink.negotiate(request))
    }

    pub(crate) fn ensure_with(
        &self,
        request: &FeatureRequest,
        initialize: impl FnOnce(&mut RemoteCommandSink, &FeatureRequest) -> hl_gpu::Result<Capabilities>,
    ) -> Result<Ready, InitializeError> {
        let mut initialize = Some(initialize);
        loop {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            match &mut *state {
                Initialization::Ready(ready) => {
                    if ready.request != *request {
                        return Err(InitializeError(
                            format!(
                                "GPU transport is already negotiated for {:?}, not {:?}",
                                ready.request, request
                            )
                            .into(),
                        ));
                    }
                    return Ok(ready.clone());
                }
                Initialization::Failed(error) => return Err(error.clone()),
                Initialization::Initializing => {
                    drop(
                        self.changed
                            .wait(state)
                            .unwrap_or_else(|error| error.into_inner()),
                    );
                }
                Initialization::Uninitialized(sink) => {
                    let requested = request.clone();
                    let mut sink = sink
                        .take()
                        .expect("uninitialized display owns its configured sink");
                    *state = Initialization::Initializing;
                    drop(state);
                    let result = initialize_display(
                        initialize.take().expect("initializer runs once"),
                        &mut sink,
                        request,
                    )
                    .map_err(|()| InitializeError("GPU transport initializer panicked".into()))
                    .and_then(|result| {
                        result.map_err(|error| {
                            InitializeError(
                                format!("GPU transport negotiation failed: {error}").into(),
                            )
                        })
                    })
                    .and_then(|capabilities| {
                        Sequencer::spawn(sink)
                            .map(|sequencer| Ready {
                                capabilities,
                                request: requested,
                                sequencer,
                            })
                            .map_err(|error| {
                                InitializeError(
                                    format!("GPU transport actor failed to start: {error}").into(),
                                )
                            })
                    });
                    let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                    *state = match &result {
                        Ok(ready) => Initialization::Ready(ready.clone()),
                        Err(error) => Initialization::Failed(error.clone()),
                    };
                    self.changed.notify_all();
                    return result;
                }
            }
        }
    }
}

#[cfg(panic = "unwind")]
fn initialize_display<F>(
    initialize: F,
    sink: &mut RemoteCommandSink,
    request: &FeatureRequest,
) -> Result<hl_gpu::Result<Capabilities>, ()>
where
    F: FnOnce(&mut RemoteCommandSink, &FeatureRequest) -> hl_gpu::Result<Capabilities>,
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| initialize(sink, request)))
        .map_err(|_| ())
}

#[cfg(panic = "abort")]
fn initialize_display<F>(
    initialize: F,
    sink: &mut RemoteCommandSink,
    request: &FeatureRequest,
) -> Result<hl_gpu::Result<Capabilities>, ()>
where
    F: FnOnce(&mut RemoteCommandSink, &FeatureRequest) -> hl_gpu::Result<Capabilities>,
{
    Ok(initialize(sink, request))
}

#[cfg(test)]
mod tests {
    use super::super::actor::Plan;
    use super::*;
    use hl_gpu::{serve_connection, Cmd, GpuError, WIRE_VERSION};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn display() -> Arc<DisplayTransport> {
        Arc::new(DisplayTransport::new(RemoteCommandSink::new(
            "/tmp/hl-gl-transport-init-test-unused.sock",
        )))
    }

    #[test]
    fn concurrent_ensure_initializes_once_and_reuses_ready() {
        let display = display();
        let calls = Arc::new(AtomicUsize::new(0));
        let threads = (0..16)
            .map(|_| {
                let display = Arc::clone(&display);
                let calls = Arc::clone(&calls);
                thread::spawn(move || {
                    display
                        .ensure_with(&FeatureRequest::default(), move |_, _| {
                            calls.fetch_add(1, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(10));
                            Ok(Capabilities::full("test"))
                        })
                        .expect("ready")
                })
            })
            .collect::<Vec<_>>();
        let ready = threads
            .into_iter()
            .map(|thread| thread.join().expect("initializer"))
            .collect::<Vec<_>>();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let first = ready[0]
            .sequencer
            .submit(Plan::new(|_| Ok(())))
            .expect("first");
        assert_eq!(first.serial().get(), 1);
        first.wait().expect("first result");
        let reused = display
            .ensure_with(&FeatureRequest::default(), |_, _| panic!("reinitialized"))
            .expect("reused");
        assert_eq!(reused.capabilities.name, "test");
        assert_eq!(reused.request, FeatureRequest::default());
        let second = reused
            .sequencer
            .submit(Plan::new(|_| Ok(())))
            .expect("second");
        assert_eq!(second.serial().get(), 2);
        second.wait().expect("second result");
    }

    #[test]
    fn failure_is_stable_and_never_reinitializes() {
        let display = display();
        let calls = AtomicUsize::new(0);
        let first = match display.ensure_with(&FeatureRequest::default(), |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(GpuError::Invalid("expected initialization failure"))
        }) {
            Err(error) => error,
            Ok(_) => panic!("initialization unexpectedly succeeded"),
        };
        let later = match display.ensure_with(&FeatureRequest::default(), |_, _| panic!("retried"))
        {
            Err(error) => error,
            Ok(_) => panic!("failed initialization was not stable"),
        };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first, later);
        assert!(later
            .to_string()
            .contains("expected initialization failure"));
    }

    #[test]
    fn ready_rejects_a_different_feature_request() {
        let display = display();
        let initial = FeatureRequest::default();
        display
            .ensure_with(&initial, |_, _| Ok(Capabilities::full("test")))
            .expect("initial request");
        let different = FeatureRequest {
            wire_version: 1,
            ..initial
        };

        let error = match display.ensure_with(&different, |_, _| panic!("renegotiated")) {
            Err(error) => error,
            Ok(_) => panic!("different request reused incompatible transport"),
        };

        assert!(error.to_string().contains("already negotiated"));
        assert!(display
            .ensure_with(&FeatureRequest::default(), |_, _| panic!("renegotiated"))
            .is_ok());
    }

    struct Socket(PathBuf);

    impl Socket {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hl-gl-display-transport-{}-{:?}.sock",
                std::process::id(),
                thread::current().id()
            ));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for Socket {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn unix_transport_negotiates_once_and_reuses_the_connection() {
        let socket = Socket::new();
        let listener = UnixListener::bind(&socket.0).expect("listener");
        let probe = listener.try_clone().expect("listener probe");
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            accepted_tx.send(()).expect("accepted");
            serve_connection(&stream, &Capabilities::full("host"), move |_, batch| {
                batch_tx.send(batch.to_vec()).expect("batch");
                true
            })
            .expect("serve connection");
        });
        let display = DisplayTransport::new(RemoteCommandSink::new(
            socket.0.to_string_lossy().into_owned(),
        ));
        let request = FeatureRequest {
            wire_version: WIRE_VERSION,
            ..FeatureRequest::default()
        };

        let first = display.ensure(&request).expect("first negotiation");
        accepted_rx.recv().expect("server accepted");
        let second = display.ensure(&request).expect("ready reuse");
        assert_eq!(first.request, request);
        assert_eq!(second.request, request);
        assert_eq!(second.capabilities.name, "host");
        first
            .sequencer
            .submit(Plan::new(|sink| sink.submit(&[Cmd::CreateFence(7)])))
            .expect("sequenced submission")
            .wait()
            .expect("acknowledged submission");
        assert_eq!(
            batch_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("batch"),
            [Cmd::CreateFence(7)]
        );

        probe.set_nonblocking(true).expect("nonblocking listener");
        assert_eq!(
            probe.accept().expect_err("no second connection").kind(),
            std::io::ErrorKind::WouldBlock
        );

        let terminal = first.sequencer.shutdown();
        assert_eq!(terminal, super::super::actor::Shutdown::Stopped);
        assert_eq!(first.sequencer.shutdown(), terminal);
        server.join().expect("server");
    }
}
