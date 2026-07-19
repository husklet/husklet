//! GPU driver composition for a workspace launch.
//!
//! Driver crates own guest-library injection and API-specific configuration. This module is the one
//! product-level place that decides which drivers a workspace combines.

use std::io;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Cmd, ConnectionHandler, CpuExecutor, GlobalLedger, GpuExecutor, Limits,
    ReadbackRequest, Session, SystemClock,
};

/// Product-owned guest injection translated into the container domain's neutral mounts and process
/// environment. Surface crates remain independent of container implementations.
pub struct Injection {
    pub mounts: Vec<hl_container::Mount>,
    pub environment: Vec<(String, String)>,
    pub library_path: Option<String>,
    pub service: Option<Service>,
    pub compositor: Option<crate::runtime::compositor::Service>,
}

impl Injection {
    pub fn for_workspace(workspace: &crate::config::WorkspaceConfig) -> io::Result<Self> {
        let enabled = workspace.gui || workspace.cuda.is_some();
        if !enabled {
            return Ok(Self {
                mounts: Vec::new(),
                environment: Vec::new(),
                library_path: None,
                service: None,
                compositor: None,
            });
        }
        let token: String = workspace
            .name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let root = crate::paths::hl_root();
        let socket =
            crate::paths::run_dir().join(format!("gpu-{token}-{}.sock", std::process::id()));
        let wayland =
            crate::paths::run_dir().join(format!("wayland-{token}-{}", std::process::id()));
        let service = Some(Service::start(&socket, Configuration::configured()?)?);
        let compositor = workspace
            .gui
            .then(|| {
                crate::runtime::compositor::Service::start_with(
                    &wayland,
                    workspace.storage_dir(&root).join("frames"),
                    crate::runtime::compositor::Presentation::configured()?,
                )
            })
            .transpose()?;
        let (arch, library) = match workspace.arch {
            hl_ws::Arch::Arm64 => ("aarch64", "/usr/lib/aarch64-linux-gnu"),
            hl_ws::Arch::Amd64 => ("x86_64", "/usr/lib/x86_64-linux-gnu"),
        };
        let mut mounts = vec![hl_container::Mount::read_write(&socket, "/run/hl-gpu.sock")];
        let mut environment = vec![("HL_GPU_EXEC".to_owned(), "/run/hl-gpu.sock".to_owned())];
        if workspace.gui {
            for (family, source, target) in [
                ("gl", "libEGL.so.1", "libEGL.so.1"),
                ("gl", "libEGL.so.1", "libEGL.so"),
                ("gl", "libGLESv2.so.2", "libGLESv2.so.2"),
                ("gl", "libGLESv2.so.2", "libGLESv2.so"),
                ("vulkan", "libvk_hl.so.1", "libvk_hl.so.1"),
                ("vulkan", "libvk_hl.so.1", "libvk_hl.so"),
                ("vulkan", "icd.json", "hl_vulkan_icd.json"),
            ] {
                mounts.push(hl_container::Mount::read_only(
                    root.join(family).join(arch).join(source),
                    format!("{library}/{target}"),
                ));
            }
            mounts.push(hl_container::Mount::read_write(&wayland, "/run/wayland-0"));
            environment.extend([
                ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
                ("XDG_RUNTIME_DIR".to_owned(), "/run".to_owned()),
                (
                    "VK_ICD_FILENAMES".to_owned(),
                    format!("{library}/hl_vulkan_icd.json"),
                ),
            ]);
        }
        if let Some(cuda) = &workspace.cuda {
            for (family, library_name) in [
                ("cuda", "libcuda.so.1"),
                ("cuda", "libcudart.so.1"),
                ("nvml", "libnvidia-ml.so.1"),
            ] {
                mounts.push(hl_container::Mount::read_only(
                    root.join(family).join(arch).join(library_name),
                    format!("{library}/{library_name}"),
                ));
            }
            environment.extend([
                ("HL_CUDA_NAME".to_owned(), cuda.name.clone()),
                ("HL_CUDA_CC".to_owned(), cuda.compute_capability.clone()),
                (
                    "HL_CUDA_VRAM_BYTES".to_owned(),
                    u64::from(cuda.vram_mb)
                        .saturating_mul(1024 * 1024)
                        .to_string(),
                ),
            ]);
        }
        Ok(Self {
            mounts,
            environment,
            library_path: Some(library.to_owned()),
            service,
            compositor,
        })
    }
}

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

impl Endpoint {
    fn serve(listener: UnixListener, stop: Arc<AtomicBool>, backend: Backend, trace: bool) {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => Self::serve_connection(stream, backend, trace),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    }

    fn serve_connection(stream: std::os::unix::net::UnixStream, backend: Backend, trace: bool) {
        thread::spawn(move || {
            let Ok(executor) = backend.executor() else {
                return;
            };
            let capabilities = executor.capabilities();
            let mut connection = Connection::new(executor, trace);
            let _ = hl_gpu::serve_connection_with_handler(&stream, &capabilities, &mut connection);
        });
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = Service::remove_socket(&self.socket);
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
}
