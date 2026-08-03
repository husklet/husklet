//! Host GPU protocol service and per-client execution sessions.

use std::io;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Cmd, ConnectionHandler, ExportId, Exports, FenceId, GlobalLedger, GpuExecutor,
    Limits, ReadbackRequest, Session, SystemClock,
};

use super::capture;
use super::executor::{Executor, Executors};
#[cfg(target_os = "macos")]
use crate::runtime::presentation::producer::Producer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Wgpu,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Wgpu => "wgpu",
        }
    }
}

impl FromStr for Backend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "cpu" => Ok(Self::Cpu),
            "wgpu" => Ok(Self::Wgpu),
            _ => Err(format!(
                "unsupported GPU backend {value:?}; expected cpu or wgpu"
            )),
        }
    }
}

pub struct Configuration {
    backend: Backend,
    trace: bool,
}

impl Configuration {
    pub fn new(backend: Backend, trace: bool) -> Self {
        Self { backend, trace }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn trace(&self) -> bool {
        self.trace
    }

    pub fn configured() -> io::Result<Self> {
        let backend = match std::env::var("HL_GPU_BACKEND") {
            Ok(value) => value
                .parse()
                .map_err(|error: String| io::Error::new(io::ErrorKind::InvalidInput, error)),
            Err(std::env::VarError::NotPresent) => Ok(Backend::Wgpu),
            Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HL_GPU_BACKEND is not valid UTF-8",
            )),
        }?;
        Ok(Self::new(
            backend,
            std::env::var_os("HL_GPU_TRACE").is_some(),
        ))
    }
}

struct Connection {
    session: Session,
    executor: Executor,
    submits: u64,
    trace: bool,
    capture: Option<capture::Capture>,
    #[cfg(target_os = "macos")]
    presentations: Option<Producer>,
}

impl Connection {
    fn new(
        executor: Executor,
        exports: Exports,
        trace: bool,
        capture: Option<capture::Capture>,
        #[cfg(target_os = "macos")] presentations: Option<Producer>,
    ) -> Self {
        let limits = Limits::from_capabilities(executor.capabilities());
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(SystemClock::new()),
        )
        .with_exports(exports);
        Self {
            session,
            executor,
            submits: 0,
            trace,
            capture,
            #[cfg(target_os = "macos")]
            presentations,
        }
    }
}

impl ConnectionHandler for Connection {
    fn submit(&mut self, header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        self.submits = self.submits.saturating_add(1);
        let submit = self.submits;
        let encode_started = Instant::now();
        let encoded = self
            .capture
            .as_ref()
            .filter(|capture| capture.active())
            .map(|_| hl_gpu::Encoder::stream(batch));
        let bytes = header.len as usize;
        let encode_elapsed = encode_started.elapsed();
        let uploaded_bytes = batch
            .iter()
            .map(|command| match command {
                Cmd::WriteBuffer { data, .. } => data.len(),
                _ => 0,
            })
            .sum::<usize>();
        if self.trace {
            eprintln!(
                "husklet gpu: submit begin commands={} encoded_bytes={}",
                batch.len(),
                bytes
            );
        }
        #[cfg(target_os = "macos")]
        let native_reservations = self
            .presentations
            .as_ref()
            .map(|producer| producer.reservations(&self.session, batch))
            .unwrap_or_default();
        let execute_started = Instant::now();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.executor, bytes, batch) {
            Ok(presentations) => {
                if let (Some(capture), Some(encoded)) = (&mut self.capture, encoded.as_deref()) {
                    capture.record(batch, encoded);
                }
                hl_log::hl_add!(
                    hl_log::tag::PRESENT,
                    "native_frames",
                    presentations.len() as u64
                );
                #[cfg(target_os = "macos")]
                if let Some(producer) = &mut self.presentations {
                    // A reservation that produced no presentation is neither published nor cancelled,
                    // and until now said nothing at all. The compositor is holding a commit deferred on
                    // that exact `(token, serial)`; with no frame and no cancellation it parks FOREVER,
                    // which is indistinguishable from a commit that was never made. Measured against
                    // Chromium: `joined=0`, one commit outstanding, `oldest_ms` climbing past eighty
                    // seconds, and no diagnostic anywhere on the path that dropped it.
                    //
                    // Reported, not cancelled. Cancelling would stop the wedge but would also decide
                    // that a presentation can never arrive later, and that is a claim about the
                    // executor this line is not in a position to make. Naming it is what lets the next
                    // measurement say which.
                    let produced: Vec<(u64, u64)> = presentations
                        .iter()
                        .map(|p| (p.token.get(), p.serial.get()))
                        .collect();
                    for reserved in &native_reservations {
                        if !produced.contains(reserved) {
                            hl_log::hl_error!(
                                hl_log::tag::PRESENT,
                                "native presentation reserved but not produced token={} serial={} \
                                 batch_commands={} presentations={} — the compositor's commit for this \
                                 token/serial has nothing to join and will stay deferred",
                                reserved.0,
                                reserved.1,
                                batch.len(),
                                presentations.len()
                            );
                        }
                    }
                    producer.publish(&self.session, &self.executor, presentations);
                }
                hl_log::hl_log!(
                    hl_log::tag::GPU,
                    hl_log::Level::Debug,
                    "submit commands={} sequence={} encoded_bytes={} uploaded_bytes={} capture_encode_us={} execute_us={} verdict=ack",
                    batch.len(),
                    submit,
                    bytes,
                    uploaded_bytes,
                    encode_elapsed.as_micros(),
                    execute_started.elapsed().as_micros()
                );
                if self.trace {
                    eprintln!("husklet gpu: submit ack");
                }
                Verdict::Ack
            }
            Err(error) => {
                #[cfg(target_os = "macos")]
                if let Some(producer) = &self.presentations {
                    producer.cancel(&native_reservations);
                }
                // A rejected batch is a guest-input outcome, not a host fault: the runtime already rolled the
                // frame back, so the session stays serving and only THIS submit fails. Report it at `error`
                // — `warn` is compiled out of a release bundle, which is why this was previously invisible
                // without the raw `eprintln!` beside it.
                hl_log::hl_error!(
                    hl_log::tag::GPU,
                    "submit commands={} sequence={} encoded_bytes={} uploaded_bytes={} capture_encode_us={} execute_us={} verdict=nack error={error}",
                    batch.len(),
                    submit,
                    bytes,
                    uploaded_bytes,
                    encode_elapsed.as_micros(),
                    execute_started.elapsed().as_micros()
                );
                // Classify the refusal from the typed error rather than collapsing it to a bare "no".
                // The acknowledgement byte is the only thing the guest receives, so a reason discarded
                // here is a reason the guest can never recover — which is why hundreds of refusals
                // reached applications as an unexplained lost device.
                Verdict::for_error(&error)
            }
        }
    }

    fn read_buffer(&mut self, request: &ReadbackRequest) -> Option<Vec<u8>> {
        let started = Instant::now();
        hl_log::hl_count!(hl_log::tag::PRESENT, "host_readbacks");
        hl_log::hl_add!(hl_log::tag::PRESENT, "host_readback_bytes", request.len);
        if self.trace {
            eprintln!(
                "husklet gpu: readback begin buffer={} offset={} len={}",
                request.id, request.offset, request.len
            );
        }
        match hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.executor,
            BufferId(request.id),
            request.offset,
            request.len as usize,
        ) {
            Ok(bytes) => {
                hl_log::hl_log!(
                    hl_log::tag::GPU,
                    hl_log::Level::Debug,
                    "readback buffer={} offset={} requested_bytes={} received_bytes={} elapsed_us={} verdict=ack",
                    request.id,
                    request.offset,
                    request.len,
                    bytes.len(),
                    started.elapsed().as_micros()
                );
                if self.trace {
                    eprintln!("husklet gpu: readback complete bytes={}", bytes.len());
                }
                Some(bytes)
            }
            Err(error) => {
                // Same shape as a rejected submit: the request fails, the connection keeps serving, and the
                // reason has to survive a release build.
                hl_log::hl_error!(
                    hl_log::tag::GPU,
                    "readback buffer={} offset={} requested_bytes={} elapsed_us={} verdict=nack error={error}",
                    request.id,
                    request.offset,
                    request.len,
                    started.elapsed().as_micros()
                );
                None
            }
        }
    }

    fn poll_fence(&mut self, request: &ReadbackRequest) -> Option<bool> {
        hl_gpu::runtime::service::dispatch::poll_fence(
            &self.session,
            &mut self.executor,
            FenceId(request.id),
            request.offset,
        )
        .ok()
    }

    fn wait_fence(&mut self, request: &ReadbackRequest) -> Option<hl_gpu::FenceWait> {
        hl_gpu::runtime::service::dispatch::wait_timeout(
            &mut self.session,
            &mut self.executor,
            FenceId(request.id),
            request.offset,
            request.len,
        )
        .ok()
    }

    fn export_buffer(&mut self, request: &ReadbackRequest) -> Option<ExportId> {
        hl_gpu::runtime::service::dispatch::export_buffer(
            &mut self.session,
            &self.executor,
            BufferId(request.id),
        )
        .ok()
    }

    fn import_buffer(&mut self, request: &ReadbackRequest) -> Option<u64> {
        hl_gpu::runtime::service::dispatch::import_buffer(
            &mut self.session,
            &self.executor,
            BufferId(request.id),
            ExportId(request.offset),
        )
        .ok()
    }
}

/// A ready GPU protocol endpoint owned by the application composition root.
pub struct Service {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start(socket: impl Into<PathBuf>, configuration: Configuration) -> io::Result<Self> {
        Self::start_with_publisher(
            socket.into(),
            configuration,
            #[cfg(target_os = "macos")]
            None,
        )
    }

    #[cfg(target_os = "macos")]
    pub fn start_native(
        socket: impl Into<PathBuf>,
        configuration: Configuration,
        presentations: hl_compositor::adapter::smithay::NativeFrameSender,
    ) -> io::Result<Self> {
        Self::start_with_publisher(socket.into(), configuration, Some(presentations))
    }

    fn start_with_publisher(
        socket: PathBuf,
        configuration: Configuration,
        #[cfg(target_os = "macos")] presentations: Option<
            hl_compositor::adapter::smithay::NativeFrameSender,
        >,
    ) -> io::Result<Self> {
        let Configuration { backend, trace } = configuration;
        let capture = capture::Config::configured()?;
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::remove_socket(&socket)?;

        // Acquire and retain the configured host device before publishing the endpoint. Each accepted
        // connection receives isolated guest state while reusing this device and queue.
        let executors = Executors::new(backend, {
            #[cfg(target_os = "macos")]
            {
                presentations.is_some()
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        })
        .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;

        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            Endpoint::serve(
                listener,
                thread_stop,
                executors,
                trace,
                capture,
                #[cfg(target_os = "macos")]
                presentations,
            )
        });

        Ok(Self {
            socket,
            stop,
            thread: Some(thread),
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn remove_socket(path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct Endpoint;

struct Connections {
    active: Arc<AtomicUsize>,
    limit: usize,
}

struct ConnectionLease(Arc<AtomicUsize>);

impl Connections {
    fn new(limit: usize) -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    fn acquire(&self) -> Option<ConnectionLease> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .ok()?;
        Some(ConnectionLease(Arc::clone(&self.active)))
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Endpoint {
    fn serve(
        listener: UnixListener,
        stop: Arc<AtomicBool>,
        executors: Executors,
        trace: bool,
        capture: Option<capture::Config>,
        #[cfg(target_os = "macos")] presentations: Option<
            hl_compositor::adapter::smithay::NativeFrameSender,
        >,
    ) {
        let connections = Connections::new(256);
        // Cross-API resource handles have process-wide identity and lifetime. Every accepted GPU
        // connection must therefore see this one registry; constructing a registry per connection would
        // compile while making every CUDA/GL or CUDA/Vulkan import fail as an unknown export.
        let exports = Exports::new();
        if trace {
            eprintln!("husklet gpu: listener ready");
        }
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if trace {
                        eprintln!("husklet gpu: connection accepted");
                    }
                    let Some(lease) = connections.acquire() else {
                        hl_log::hl_error!(
                            hl_log::tag::GPU,
                            "GPU connection rejected active_limit={}",
                            connections.limit
                        );
                        continue;
                    };
                    Self::serve_connection(
                        stream,
                        executors.clone(),
                        exports.clone(),
                        trace,
                        capture.clone(),
                        lease,
                        #[cfg(target_os = "macos")]
                        presentations.clone().map(Producer::new),
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    hl_log::hl_error!(hl_log::tag::GPU, "GPU service accept failed error={error}");
                    break;
                }
            }
        }
    }

    fn serve_connection(
        stream: std::os::unix::net::UnixStream,
        executors: Executors,
        exports: Exports,
        trace: bool,
        capture: Option<capture::Config>,
        lease: ConnectionLease,
        #[cfg(target_os = "macos")] presentations: Option<Producer>,
    ) {
        thread::spawn(move || {
            let _lease = lease;
            if trace {
                eprintln!("husklet gpu: connection handler begin");
            }
            // BSD/macOS accepted sockets inherit O_NONBLOCK from the listener. The framed GPU protocol is
            // deliberately blocking per connection; leaving this inherited makes an idle read return
            // WouldBlock, closes the server side, and presents as BrokenPipe during client negotiation.
            if let Err(error) = stream.set_nonblocking(false) {
                hl_log::hl_error!(
                    hl_log::tag::GPU,
                    "GPU connection blocking-mode setup failed error={error}"
                );
                return;
            }
            let executor = executors.executor();
            if trace {
                eprintln!("husklet gpu: connection executor ready");
            }
            let capabilities = executor.capabilities();
            static NEXT_CAPTURE: AtomicUsize = AtomicUsize::new(0);
            let capture = capture.and_then(|configuration| {
                let connection = NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed) as u64;
                match configuration.open(connection) {
                    Ok(capture) => Some(capture),
                    Err(error) => {
                        hl_log::hl_warn!(
                            hl_log::tag::GPU,
                            "GPU capture open failed connection={connection} error={error}"
                        );
                        None
                    }
                }
            });
            let mut connection = Connection::new(
                executor,
                exports,
                trace,
                capture,
                #[cfg(target_os = "macos")]
                presentations,
            );
            if trace {
                eprintln!("husklet gpu: connection protocol begin");
            }
            if let Err(error) =
                hl_gpu::serve_connection_with_handler(&stream, &capabilities, &mut connection)
            {
                hl_log::hl_error!(
                    hl_log::tag::GPU,
                    "GPU connection protocol failed error={error}"
                );
            }
        });
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                hl_log::hl_error!(hl_log::tag::GPU, "GPU service thread panicked");
            }
        }
        if let Err(error) = Service::remove_socket(&self.socket) {
            hl_log::hl_error!(
                hl_log::tag::GPU,
                "GPU service socket cleanup failed path={} error={error}",
                self.socket.display()
            );
        }
    }
}

#[cfg(test)]
#[path = "gpu/service_test.rs"]
mod tests;
