//! A `WgpuExecutor`-backed host for the REAL GRAPHICS tests — the same `$HL_GPU_EXEC` unix-socket host as
//! `common::Executor`, but every guest batch is dispatched through the runtime pipeline onto a real
//! `hl_gpu_wgpu::WgpuExecutor` bound to the headless software Vulkan device (lavapipe / `llvmpipe`) instead
//! of the fixed-function `CpuExecutor`. So a guest that lowers *graphics* IR (a render pass + a real
//! SPIR-V graphics pipeline + a draw) gets it genuinely RASTERIZED on lavapipe, and the rendered target
//! is read back off the device.
//!
//! WHY the host reads the target (not the guest): our Vulkan ICD's `vkMapMemory` is write-through — it
//! hands the app back its own staging bytes and issues NO device→host readback (see the `vk_compute.c`
//! scope note + `shim/vulkan/src/compute.rs::vkMapMemory`). So a real Vulkan app cannot observe GPU output
//! through the map path today (a real, filed shim gap). Meanwhile the pixels DO land on the host: the
//! guest's render pass writes the wgpu texture, and its `vkCmdCopyImageToBuffer` writes a wgpu buffer, both
//! behind protocol ids in this connection's `SessionResources`. This host captures those off the executor
//! after each batch, so the graphics test asserts the ACTUAL lavapipe raster output — the furthest
//! observable correct step given the map-readback gap.

use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    encode_stream, BufferId, Cmd, ConnectionHandler, FakeClock, GlobalLedger,
    GpuExecutor, Limits, ReadbackRequest, Session,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// The rendered device state this host reads back off lavapipe, keyed by protocol resource id, so the test
/// can assert what actually rasterized. Overwritten (latest-wins) after every guest batch, so the values
/// reflect the final draw even if the guest later tears resources down.
#[derive(Default, Clone)]
pub struct Captured {
    /// Texture id → tight-packed level-0 color plane (`w*h*bytes_per_texel` bytes, no row padding).
    pub textures: HashMap<u32, Vec<u8>>,
    /// Buffer id → its full contents (the guest's `vkCmdCopyImageToBuffer` destination lands here).
    pub buffers: HashMap<u32, Vec<u8>>,
}

impl Captured {
    /// The one texture whose byte length matches `w*h*4` (RGBA8). The real-app tests create exactly one
    /// render target, so this unambiguously finds it without predicting the shared-namespace IR id.
    pub fn rgba8_texture(&self, w: u32, h: u32) -> Option<&Vec<u8>> {
        let want = (w * h * 4) as usize;
        self.textures.values().find(|v| v.len() == want)
    }

    /// The one buffer whose byte length matches `w*h*4` (the copy-to-buffer readback destination).
    pub fn rgba8_buffer(&self, w: u32, h: u32) -> Option<&Vec<u8>> {
        let want = (w * h * 4) as usize;
        self.buffers.values().find(|v| v.len() == want)
    }
}

/// The process-wide lavapipe executor. Device bring-up is the expensive part and one software Vulkan
/// device serves every connection (each with its own runtime `Session`), exactly as the wgpu conformance
/// suite shares one device across its cases.
fn shared_exec() -> &'static Arc<Mutex<WgpuExecutor>> {
    static EXEC: OnceLock<Arc<Mutex<WgpuExecutor>>> = OnceLock::new();
    EXEC.get_or_init(|| {
        let mut exec = WgpuExecutor::new(DeviceConfig::default())
            .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)");
        // Injected PTX front-end: harmless for the graphics tests (no PtxKernel is ever created), present
        // so a compute guest sharing this host still compiles its kernel payload — matching `RuntimeHost`.
        exec.set_kernel_compiler(|desc: &KernelDescriptor| {
            hl_cuda::adapter::ptx::compile(&desc.ptx, &desc.entry, desc.block)
        });
        Arc::new(Mutex::new(exec))
    })
}

/// A per-connection host: the shared lavapipe executor + this connection's own runtime `Session`. Drives
/// the submit path through `validate → account → dispatch → execute` and, after each batch, captures the
/// rendered textures/buffers off the executor into the shared [`Captured`].
struct WgpuHost {
    session: Session,
    submits: Arc<AtomicU64>,
    captured: Arc<Mutex<Captured>>,
    /// Buffer id → logical size, learned from `CreateBuffer` so the post-batch capture knows how many bytes
    /// to read back for each buffer.
    buffer_sizes: HashMap<u32, u64>,
}

impl WgpuHost {
    fn new(submits: Arc<AtomicU64>, captured: Arc<Mutex<Captured>>) -> Self {
        // A permissive limit set with byte-addressable copies (matches the wgpu conformance harness), so the
        // real graphics/compute lowering — SPIR-V modules, render targets, unaligned copies — validates.
        let caps = shared_exec().lock().unwrap().capabilities();
        let mut limits = Limits::from_capabilities(caps);
        limits.copy_alignment = 1;
        let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
        Self { session, submits, captured, buffer_sizes: HashMap::new() }
    }

    /// Read every live texture/buffer off the executor and merge into the shared capture (latest-wins).
    fn capture(&self, exec: &WgpuExecutor) {
        let mut cap = self.captured.lock().unwrap();
        for (id, _) in self.session.resources.textures.iter() {
            if let Ok(px) = exec.read_texture(&self.session.resources, id) {
                cap.textures.insert(id, px);
            }
        }
        for (id, _) in self.session.resources.buffers.iter() {
            if let Some(&size) = self.buffer_sizes.get(&id) {
                if let Ok(bytes) =
                    exec.read_buffer(&self.session.resources, BufferId(id), 0, size as usize)
                {
                    cap.buffers.insert(id, bytes);
                }
            }
        }
    }
}

impl ConnectionHandler for WgpuHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        self.submits.fetch_add(1, Ordering::Relaxed);
        for cmd in batch {
            if let Cmd::CreateBuffer(id, d) = cmd {
                self.buffer_sizes.insert(*id, d.size);
            }
        }
        let frame_bytes = encode_stream(batch).len();
        let mut exec = shared_exec().lock().unwrap_or_else(|e| e.into_inner());
        match hl_gpu::runtime::submit(&mut self.session, &mut *exec, frame_bytes, batch) {
            Ok(_) => {
                // Capture whatever lavapipe just produced (rendered target, copy-out buffer) so the test
                // can assert it even though the guest's own map path can't read it back.
                self.capture(&exec);
                Verdict::Ack
            }
            Err(_) => Verdict::Nack,
        }
    }

    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        let exec = shared_exec().lock().unwrap_or_else(|e| e.into_inner());
        exec.read_buffer(&self.session.resources, BufferId(req.id), req.offset, req.len as usize)
            .ok()
    }
}

/// A running lavapipe-backed host GPU executor on a temp unix socket. Mirrors [`crate::common::Executor`]
/// but dispatches onto `WgpuExecutor` and exposes the captured render output.
pub struct WgpuExecutorServer {
    pub sock_path: PathBuf,
    stop: Arc<AtomicBool>,
    submits: Arc<AtomicU64>,
    captured: Arc<Mutex<Captured>>,
    _thread: thread::JoinHandle<()>,
}

impl WgpuExecutorServer {
    /// Bind a fresh temp socket and start accepting guest connections, each served by its own [`WgpuHost`]
    /// over the shared lavapipe device. Eagerly forces device bring-up so a failure surfaces here (not on a
    /// background thread) and so the adapter identity can be asserted before launching the guest.
    pub fn start(tag: &str) -> Self {
        // Force the (lazy) device acquisition now so a missing-ICD failure is a clean panic on the test
        // thread rather than a silent guest error.
        let _ = shared_exec();

        let sock_path = std::env::temp_dir().join(format!(
            "hl-wip-wgpu-{tag}-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind wgpu executor socket");
        listener.set_nonblocking(true).expect("nonblocking wgpu executor socket");

        let stop = Arc::new(AtomicBool::new(false));
        let submits = Arc::new(AtomicU64::new(0));
        let captured = Arc::new(Mutex::new(Captured::default()));

        let stop_t = Arc::clone(&stop);
        let submits_t = Arc::clone(&submits);
        let captured_t = Arc::clone(&captured);
        let thread = thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        let submits_c = Arc::clone(&submits_t);
                        let captured_c = Arc::clone(&captured_t);
                        thread::spawn(move || {
                            let caps = shared_exec().lock().unwrap().capabilities();
                            let mut host = WgpuHost::new(submits_c, captured_c);
                            let _ = hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { sock_path, stop, submits, captured, _thread: thread }
    }

    /// Path string for `HL_GPU_EXEC`.
    pub fn sock(&self) -> String {
        self.sock_path.to_string_lossy().into_owned()
    }

    /// Total protocol batches the guest(s) submitted so far.
    pub fn submit_count(&self) -> u64 {
        self.submits.load(Ordering::Relaxed)
    }

    /// A snapshot of the render output captured off lavapipe.
    pub fn captured(&self) -> Captured {
        self.captured.lock().unwrap().clone()
    }

    /// The bound adapter's human-readable name (e.g. `"llvmpipe (LLVM 17.0.6, 128 bits)"`), so a test can
    /// assert it landed on the software Vulkan device.
    pub fn adapter_name(&self) -> String {
        shared_exec().lock().unwrap().adapter_name().to_string()
    }
}

impl Drop for WgpuExecutorServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);
