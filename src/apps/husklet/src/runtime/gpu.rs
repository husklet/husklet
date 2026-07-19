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

use hl_cuda::{Cuda as CudaDriver, CudaSpec};
use hl_gl::{Gl, GlSpec};
use hl_jit::{DeviceProvider, DeviceRequest, Drivers};
use hl_vulkan::{Vulkan, VulkanSpec};

use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Cmd, ConnectionHandler, CpuExecutor, GlobalLedger, GpuExecutor, Limits,
    ReadbackRequest, Session, SystemClock,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestArch {
    Aarch64,
    X86_64,
}

impl GuestArch {
    fn cuda(self) -> hl_cuda::Arch {
        match self {
            Self::Aarch64 => hl_cuda::Arch::Aarch64,
            Self::X86_64 => hl_cuda::Arch::X86_64,
        }
    }

    fn gl(self) -> hl_gl::Arch {
        match self {
            Self::Aarch64 => hl_gl::Arch::Aarch64,
            Self::X86_64 => hl_gl::Arch::X86_64,
        }
    }

    fn vulkan(self) -> hl_vulkan::Arch {
        match self {
            Self::Aarch64 => hl_vulkan::Arch::Aarch64,
            Self::X86_64 => hl_vulkan::Arch::X86_64,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Display {
    pub wayland_socket: PathBuf,
    pub surface_size: Option<(u32, u32)>,
}

#[derive(Clone, Debug)]
pub struct Cuda {
    pub device: crate::config::CudaDevice,
    pub nvidia_smi: Option<PathBuf>,
}

pub struct Gpu {
    arch: GuestArch,
    stage_root: PathBuf,
    socket: PathBuf,
    display: Option<Display>,
    cuda: Option<Cuda>,
}

impl Gpu {
    pub fn new(
        arch: GuestArch,
        stage_root: impl Into<PathBuf>,
        socket: impl Into<PathBuf>,
    ) -> Self {
        Self {
            arch,
            stage_root: stage_root.into(),
            socket: socket.into(),
            display: None,
            cuda: None,
        }
    }

    pub fn with_display(mut self, display: Display) -> Self {
        self.display = Some(display);
        self
    }

    pub fn with_cuda(mut self, cuda: Cuda) -> Self {
        self.cuda = Some(cuda);
        self
    }

    pub fn is_inert(&self) -> bool {
        self.display.is_none() && self.cuda.is_none()
    }

    fn drivers(&self) -> Drivers {
        let mut drivers = Drivers::new();

        if let Some(display) = &self.display {
            let mut gl = GlSpec::new(self.arch.gl(), &self.socket).stage_root(&self.stage_root);
            if let Some((width, height)) = display.surface_size {
                gl = gl.surface_size(width, height);
            }
            drivers.add(Gl::new(gl));
            drivers.add(Vulkan::new(
                VulkanSpec::new(self.arch.vulkan(), &self.socket).stage_root(&self.stage_root),
            ));
        }

        if let Some(cuda) = &self.cuda {
            let bytes = u64::from(cuda.device.vram_mb).saturating_mul(1024 * 1024);
            let spec = CudaSpec::new(self.arch.cuda(), &self.socket)
                .stage_root(&self.stage_root)
                .advertise(&cuda.device.name, &cuda.device.compute_capability, bytes);
            drivers.add(CudaDriver::new(spec));
        }

        drivers
    }
}

impl DeviceProvider for Gpu {
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest {
        // The render node belongs to the composed display/GPU service, not to any one API shim. GL and
        // Vulkan only contribute guest libraries; when a display is present Husklet also asks the engine
        // for the shared host-backed allocation device those libraries and Wayland clients discover.
        let mut request = DeviceRequest {
            render_node: self.display.is_some(),
            ..DeviceRequest::default()
        };
        let mut env = guest_env.to_vec();

        for driver in self.drivers().requests(&env) {
            request.mounts.extend(driver.mounts);
            request.render_node |= driver.render_node;
            env.extend(driver.env.iter().cloned());
            request.env.extend(driver.env);
        }

        if let Some(tool) = self.cuda.as_ref().and_then(|cuda| cuda.nvidia_smi.as_ref()) {
            request.mounts.push(hl_jit::DeviceMount::ro(
                tool.to_string_lossy().into_owned(),
                "/usr/local/bin/nvidia-smi",
            ));
        }

        if let Some(display) = &self.display {
            request.mounts.push(hl_jit::DeviceMount::rw(
                display.wayland_socket.to_string_lossy().into_owned(),
                "/run/wayland-0",
            ));
            request.env.push("WAYLAND_DISPLAY=wayland-0".into());
            request.env.push("XDG_RUNTIME_DIR=/run".into());
        }

        request
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
    fn display_composes_gl_vulkan_and_wayland() {
        let gpu =
            Gpu::new(GuestArch::X86_64, "/stage", "/host/hl-gpu.sock").with_display(Display {
                wayland_socket: "/host/wayland-0".into(),
                surface_size: Some((800, 600)),
            });
        let request = gpu.device_request(&["LD_LIBRARY_PATH=/usr/lib".into()]);

        assert!(request
            .env
            .iter()
            .any(|line| line == "WAYLAND_DISPLAY=wayland-0"));
        assert!(request.env.iter().any(|line| line == "HL_GL_SURFACE_W=800"));
        assert!(request
            .env
            .iter()
            .any(|line| line.starts_with("VK_ICD_FILENAMES=")));
        assert!(request
            .mounts
            .iter()
            .any(|mount| mount.container.ends_with("libEGL.so.1")));
        assert!(request
            .mounts
            .iter()
            .any(|mount| mount.container.ends_with("libvk_hl.so.1")));
        assert!(
            request.render_node,
            "a composed display needs the shared host-backed render node"
        );
    }

    #[test]
    fn headless_cuda_does_not_attach_display_drivers() {
        let gpu = Gpu::new(GuestArch::Aarch64, "/stage", "/host/hl-gpu.sock").with_cuda(Cuda {
            device: crate::config::CudaDevice {
                name: "test".into(),
                compute_capability: "8.6".into(),
                vram_mb: 512,
            },
            nvidia_smi: None,
        });
        let request = gpu.device_request(&[]);

        assert!(request
            .env
            .iter()
            .any(|line| line == "HL_CUDA_VRAM_BYTES=536870912"));
        assert!(!request
            .env
            .iter()
            .any(|line| line.starts_with("VK_ICD_FILENAMES=")));
        assert!(!request
            .env
            .iter()
            .any(|line| line.starts_with("WAYLAND_DISPLAY=")));
        assert!(
            !request.render_node,
            "headless CUDA must not enable the display allocation device"
        );
    }

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
