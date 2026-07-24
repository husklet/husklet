//! Host GPU protocol service and per-client execution sessions.

use std::io;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Cmd, ConnectionHandler, CpuExecutor, GlobalLedger, GpuExecutor, Limits,
    ReadbackRequest, Session, SystemClock,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Wgpu,
}

pub struct Configuration {
    backend: Backend,
    trace: bool,
}

impl Configuration {
    pub fn configured() -> io::Result<Self> {
        let backend = match std::env::var("HL_GPU_BACKEND").as_deref() {
            Ok("cpu") => Ok(Backend::Cpu),
            Ok("wgpu") | Err(std::env::VarError::NotPresent) => Ok(Backend::Wgpu),
            Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HL_GPU_BACKEND is not valid UTF-8",
            )),
            Ok(value) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported HL_GPU_BACKEND {value:?}; expected cpu or wgpu"),
            )),
        }?;
        Ok(Self {
            backend,
            trace: std::env::var_os("HL_GPU_TRACE").is_some(),
        })
    }
}

impl Backend {
    fn executor(self) -> Result<Box<dyn GpuExecutor>, String> {
        match self {
            Self::Cpu => {
                let mut executor = CpuExecutor::new();
                executor.set_kernel_compiler(KernelCompiler::compile);
                Ok(Box::new(executor))
            }
            Self::Wgpu => {
                let mut executor = hl_gpu_wgpu::WgpuExecutor::new(Default::default())
                    .map_err(|error| error.to_string())?;
                executor.set_kernel_compiler(KernelCompiler::compile);
                Ok(Box::new(executor))
            }
        }
    }
}

struct KernelCompiler;

impl KernelCompiler {
    fn compile(descriptor: &KernelDescriptor) -> hl_gpu::Result<KernelProgram> {
        hl_cuda::adapter::ptx::compile(&descriptor.ptx, &descriptor.entry, descriptor.block)
    }
}

struct Connection {
    session: Session,
    executor: Box<dyn GpuExecutor>,
    trace: bool,
}

impl Connection {
    fn new(executor: Box<dyn GpuExecutor>, trace: bool) -> Self {
        let limits = Limits::from_capabilities(executor.capabilities());
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(SystemClock::new()),
        );
        Self {
            session,
            executor,
            trace,
        }
    }
}

impl ConnectionHandler for Connection {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        let bytes = hl_gpu::Encoder::stream(batch).len();
        if self.trace {
            eprintln!(
                "husklet gpu: submit begin commands={} encoded_bytes={}",
                batch.len(),
                bytes
            );
        }
        match hl_gpu::runtime::submit(&mut self.session, self.executor.as_mut(), bytes, batch) {
            Ok(_) => {
                if self.trace {
                    eprintln!("husklet gpu: submit ack");
                }
                Verdict::Ack
            }
            Err(error) => {
                eprintln!(
                    "husklet gpu: rejected submit (commands={}, encoded_bytes={}): {error}",
                    batch.len(),
                    bytes
                );
                Verdict::Nack
            }
        }
    }

    fn read_buffer(&mut self, request: &ReadbackRequest) -> Option<Vec<u8>> {
        if self.trace {
            eprintln!(
                "husklet gpu: readback begin buffer={} offset={} len={}",
                request.id, request.offset, request.len
            );
        }
        match hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            self.executor.as_ref(),
            BufferId(request.id),
            request.offset,
            request.len as usize,
        ) {
            Ok(bytes) => {
                if self.trace {
                    eprintln!("husklet gpu: readback complete bytes={}", bytes.len());
                }
                Some(bytes)
            }
            Err(error) => {
                eprintln!(
                    "husklet gpu: readback failed (buffer={}, offset={}, len={}): {error}",
                    request.id, request.offset, request.len
                );
                None
            }
        }
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
        let socket = socket.into();
        let Configuration { backend, trace } = configuration;
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::remove_socket(&socket)?;

        // Fail before publishing the endpoint when the configured executor is unavailable.
        backend
            .executor()
            .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;

        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || Endpoint::serve(listener, thread_stop, backend, trace));

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
    fn serve(listener: UnixListener, stop: Arc<AtomicBool>, backend: Backend, trace: bool) {
        let connections = Connections::new(256);
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let Some(lease) = connections.acquire() else {
                        hl_log::hl_error!(
                            hl_log::tag::GPU,
                            "GPU connection rejected active_limit={}",
                            connections.limit
                        );
                        continue;
                    };
                    Self::serve_connection(stream, backend, trace, lease);
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
        backend: Backend,
        trace: bool,
        lease: ConnectionLease,
    ) {
        thread::spawn(move || {
            let _lease = lease;
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
            let executor = match backend.executor() {
                Ok(executor) => executor,
                Err(error) => {
                    hl_log::hl_error!(
                        hl_log::tag::GPU,
                        "GPU connection executor unavailable error={error}"
                    );
                    return;
                }
            };
            let capabilities = executor.capabilities();
            let mut connection = Connection::new(executor, trace);
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
mod tests {
    use super::*;

    #[test]
    fn service_publishes_and_removes_a_ready_endpoint() {
        let path = std::env::temp_dir().join(format!("hl-gpu-service-{}.sock", std::process::id()));
        let service = Service::start(
            &path,
            Configuration {
                backend: Backend::Cpu,
                trace: false,
            },
        )
        .unwrap();
        assert_eq!(service.socket(), path);
        assert!(path.exists());

        let mut sink = hl_gpu::RemoteCommandSink::new(path.to_string_lossy());
        use hl_gpu::CommandSink as _;
        sink.negotiate(&hl_gpu::FeatureRequest {
            wire_version: hl_gpu::protocol::WIRE_VERSION,
            shader_payloads: 0,
            command_bits: 0,
            texture_formats: 0,
        })
        .unwrap();
        sink.submit(&[]).unwrap();

        drop(service);
        assert!(!path.exists());
    }

    #[test]
    fn connection_capacity_is_bounded_and_released() {
        let connections = Connections::new(2);
        let first = connections.acquire().unwrap();
        let second = connections.acquire().unwrap();
        assert!(connections.acquire().is_none());

        drop(first);
        let replacement = connections.acquire().unwrap();
        assert!(connections.acquire().is_none());

        drop(second);
        drop(replacement);
        assert_eq!(connections.active.load(Ordering::Acquire), 0);
    }
}
