//! `hl_gpu_wgpu` — the cross-platform GPU executor: `impl hl_gpu::runtime::port::GpuExecutor` on wgpu.
//!
//! This is the real-shader-executing counterpart to the crate's CPU reference oracle. It stores native
//! wgpu resources behind the protocol ids in [`SessionResources`] (one native object per id, exactly as
//! `CpuExecutor` does), lowers the neutral kernel-IR and SPIR-V/GLSL shader payloads to WGSL host-side via
//! naga, and runs them on whatever adapter wgpu binds. On this headless Linux host that adapter is the
//! software Vulkan device (lavapipe / `llvmpipe`), so the frozen conformance suite — including a real
//! SPIR-V vertex+fragment triangle and the vecadd compute kernel the CPU can only *interpret* — executes
//! end to end with no GPU and no display.
//!
//! The crate *is* the backend: no `backend/` folder, one native concern per single-word file
//! (`buffer`/`texture`/`sampler`/`shader`/`pipeline`/`bindgroup`/`fence`), `submit` records encoder work,
//! `present`/`convert`/`wgsl` do the IR→wgpu maps and shader translation, `device` acquires the adapter,
//! and `executor` is the thin `GpuExecutor` router.
//!
//! ```no_run
//! use hl_gpu_wgpu::{WgpuExecutor, DeviceConfig};
//! let mut exec = WgpuExecutor::new(DeviceConfig::default()).unwrap();
//! println!("bound adapter: {}", exec.adapter_name());
//! ```

mod bc1;
mod bindgroup;
mod blit;
mod buffer;
mod convert;
mod dedup;
mod device;
mod executor;
mod fence;
mod glsl_es;
#[cfg(target_os = "macos")]
mod iosurface;
mod module;
mod pipeline;
mod present;
mod profile;
mod reflect;
mod sampler;
mod shader;
mod spirv_split;
mod submit;
mod texel_buffer;
mod texture;
mod wgsl;

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::Mutex;

use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, PresentKind, COLOR_FORMATS, DEPTH_FORMATS,
};
use hl_gpu::protocol::model::command::{etag, WIRE_VERSION};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::Result;

pub use device::{DeviceConfig, Gpu};
pub use profile::{Metric, Profile};

/// An acquired host GPU from which isolated protocol executors can be created.
///
/// Clones retain the same wgpu device and queue. Executors created from this value share only that host
/// device: guest resource ids, kernel definitions, and compiled-object aliases remain executor-local.
#[derive(Clone)]
pub struct Device {
    gpu: Arc<Gpu>,
    modules: Arc<module::Modules>,
    pipelines: Arc<pipeline::Residency>,
    #[cfg(target_os = "macos")]
    iosurface: Option<iosurface::Allocator>,
}

impl Device {
    /// Acquire a portable headless host GPU.
    pub fn new(config: DeviceConfig) -> Result<Self> {
        Ok(Self {
            gpu: Arc::new(Gpu::acquire(&config)?),
            modules: Arc::new(module::Modules::new()),
            pipelines: Arc::new(pipeline::Residency::new()),
            #[cfg(target_os = "macos")]
            iosurface: None,
        })
    }

    /// Acquire a Metal host GPU and prove native IOSurface presentation once.
    #[cfg(target_os = "macos")]
    pub fn new_iosurface(config: DeviceConfig) -> Result<Self> {
        let gpu = Arc::new(Gpu::acquire(&config)?);
        let iosurface = Some(iosurface::Allocator::new(&gpu)?);
        Ok(Self {
            gpu,
            modules: Arc::new(module::Modules::new()),
            pipelines: Arc::new(pipeline::Residency::new()),
            iosurface,
        })
    }

    /// Create an isolated guest executor on this host device.
    pub fn executor(&self) -> WgpuExecutor {
        WgpuExecutor::from_device(self)
    }
}

/// A completed macOS presentation image retained independently of the guest texture resource.
///
/// Dropping the last clone releases the IOSurface. A compositor must retain this value until it has
/// finished importing or displaying the frame; guest texture destruction cannot invalidate it meanwhile.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct IoSurfaceImage {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    surface: hl_iosurface::Surface,
    completion: IoSurfaceCompletion,
}

/// Nonblocking GPU-production completion for an IOSurface frame.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct IoSurfaceCompletion {
    gpu: Arc<Gpu>,
    ready: Arc<AtomicBool>,
}

#[cfg(target_os = "macos")]
impl IoSurfaceCompletion {
    /// Stable identity of the host device whose queue owns this completion.
    pub fn device_key(&self) -> u64 {
        Arc::as_ptr(&self.gpu) as usize as u64
    }

    /// Service completion callbacks without waiting for unrelated GPU work.
    pub fn is_ready(&self) -> bool {
        self.gpu.device.poll(wgpu::Maintain::Poll);
        self.ready.load(Ordering::Acquire)
    }
}

#[cfg(target_os = "macos")]
impl IoSurfaceImage {
    pub fn surface(&self) -> &hl_iosurface::Surface {
        &self.surface
    }

    pub fn completion(&self) -> IoSurfaceCompletion {
        self.completion.clone()
    }
}

/// The encoder ops this executor actually replays — now EVERY command in the current IR, each with a real
/// handler (see `submit.rs`, `blit.rs`, `texture.rs`). Advertising a command it did not truly execute would
/// be a capability lie: a catch-all `_ =>` replay arm would silently no-op a submitted op, so a guest that
/// negotiated it would see its op vanish with no error and no effect. This set is the honest truth — the
/// runtime rejects any command NOT listed here at validate. `CopyTextureToTexture` (exact, no scaling),
/// `BlitTexture` (scaled/filtered, resampled by a textured-triangle draw — see `blit.rs`), and
/// `ResolveTexture` (multisample resolve via a render-pass `resolve_target`) are all implemented and
/// advertised; `CopyTextureToBuffer` reads back the exact `mip` level it names (see `texture.rs`).
const REPLAYED_COMMANDS: &[u8] = &[
    etag::BEGIN_RENDER_PASS,
    etag::END_RENDER_PASS,
    etag::SET_PIPELINE,
    etag::SET_BIND_GROUP,
    etag::SET_VERTEX_BUFFER,
    etag::SET_INDEX_BUFFER,
    etag::SET_VIEWPORT,
    etag::SET_SCISSOR,
    etag::CLEAR_RECT,
    etag::DRAW,
    etag::DRAW_INDEXED,
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
    etag::COPY_B2T,
    etag::COPY_T2B,
    etag::COPY_B2T_REGION,
    etag::COPY_T2B_REGION,
    etag::COPY_T2T,
    etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE,
    etag::FILL_BUFFER,
    etag::SET_STENCIL_REFERENCE,
    etag::SET_BLEND_CONSTANT,
];

/// Per-frame wire-byte ceiling: the largest DECODED FRAME (one batch of encoded IR, including any inline
/// `WriteBuffer` payload) this executor accepts. A hostile-guest DoS guard on the transient per-frame
/// allocation — NOT a correctness bound, and NOT a per-allocation bound (see
/// [`MAX_ADVERTISED_BUFFER_BYTES`], which is the separate single-resource ceiling; the two were previously
/// one number, which is how a frame budget came to act as a buffer ceiling).
///
/// Sized BROWSER-CLASS: the old 64 MiB tripped healthy Chrome frames, seen at 89–168 MB. It must stay
/// below the coarse transport pre-read guard (`transport::adapter::unix::MAX_FRAME_BYTES`, 512 MiB), which
/// refuses an oversized frame before a body byte is read, so this cannot follow the buffer ceiling upward.
/// Finite by construction.
const MAX_FRAME_BYTES: u64 = 256 << 20;

/// Ceiling on the single-allocation size this executor will ADVERTISE, whatever the adapter reports.
///
/// The advertised value is `min(device max_buffer_size, this)`. Metal reports `max_buffer_size` in the
/// multi-GiB range and wgpu really will serve such an allocation, but `max_buffer_bytes` is not read only
/// as a wgpu allocation bound once it is negotiated:
///
///   * the runtime derives a connection's residency ceiling from it
///     (`Limits::from_capabilities`: `2 ×`), so advertising multi-GiB hands one guest a multi-GiB pinned
///     working set before the per-connection guard engages;
///   * the GL shim sizes its host-side readback/staging groups by the negotiated value
///     (`hl-gl::service::frame::Frame::capture_targets`), so it is also a HOST-RAM budget, not only a
///     device-address-space budget.
///
/// 1 GiB is therefore what the whole path can honour, not merely what the device can allocate: it clears
/// the largest legitimate single Skia/Chrome vertex or index buffer (which does exceed 256 MiB) while
/// keeping the derived per-connection and host-staging budgets bounded. Advertising the adapter's raw
/// multi-GiB figure would be a capability claim the surrounding accounting cannot keep.
///
/// SCOPE — this is an ALLOCATION ceiling, not an upload ceiling. A buffer's CONTENTS still cross the wire
/// as `Cmd::WriteBuffer { offset, data }` inside a frame, so a guest filling a buffer larger than
/// [`MAX_FRAME_BYTES`] must split the fill across frames at increasing offsets, which the IR supports.
/// A guest driver that emits one whole-buffer `WriteBuffer` will hit `ResourceLimit("frame bytes")`
/// instead — that is the frame budget doing its own job, not this ceiling being dishonest.
const MAX_ADVERTISED_BUFFER_BYTES: u64 = 1 << 30;

/// The wgpu-backed [`hl_gpu::GpuExecutor`]. Holds the acquired device/queue, the negotiated capabilities
/// it advertises, and the kernel front-end state (a pre-registered kernel map + optional PTX-descriptor
/// compiler) that resolves a `PtxKernel` shader payload — the same seam the CPU oracle exposes.
pub struct WgpuExecutor {
    gpu: Arc<Gpu>,
    /// Whether this executor has queue writes waiting for a submission boundary.
    ///
    /// `Queue::write_buffer` preserves queue order but does not require an empty submission per write.
    /// The executor flushes the whole run once when the protocol batch ends. Any real command-buffer
    /// submission in between also makes the writes visible to the GPU in their original order.
    pending_writes: AtomicBool,
    /// Diagnostic-only lineage for Chrome's `[11 22 33 ff]` upload sentinel.
    diagnostic_sentinel_buffer: Cell<Option<u32>>,
    diagnostic_sentinel_texture: Cell<Option<u32>>,
    diagnostic_sentinel_readback: Cell<Option<u32>>,
    diagnostic_sentinel_submits: Cell<u8>,
    diagnostic_sentinel_batch_results: Cell<u8>,
    diagnostic_upload_candidates: Cell<u8>,
    modules: Arc<module::Modules>,
    /// Device-cache mutations accepted by the current protocol batch. They become visible to later
    /// connections only when the batch commits.
    module_journal: Vec<module::Mutation>,
    pipelines: Arc<pipeline::Residency>,
    /// Device-residency mutations become visible to other executors only after the current protocol batch
    /// and its guest resource-table transaction both commit.
    pipeline_journal: Vec<pipeline::Mutation>,
    caps: Capabilities,
    profile: RefCell<Option<Profile>>,
    /// Presentable host allocation selected at construction. Absence is the portable SHM path.
    #[cfg(target_os = "macos")]
    iosurface: Option<iosurface::Allocator>,
    #[cfg(target_os = "macos")]
    presentation_completions: Mutex<HashMap<(u64, u64), IoSurfaceCompletion>>,
    #[cfg(target_os = "macos")]
    presentation_journal: Vec<(u64, u64)>,
    /// `(surface token, frame serial)` retirements the current batch's presentations earn. Applied on
    /// commit so a rolled-back batch cannot discard an earlier committed frame's completion record.
    #[cfg(target_os = "macos")]
    presentation_retirements: Vec<(u64, u64)>,
    /// Test-only observation of host-blocking device waits. Keeping the counter beside the one helper that
    /// performs `Maintain::Wait` lets focused tests distinguish queue-ordered submission from CPU-visible
    /// completion without timing assertions.
    #[cfg(test)]
    completion_waits: Cell<u64>,
    /// Test observation of native texture-to-buffer queue submissions. A qualifying copy must enqueue one
    /// command buffer regardless of its height; the old CPU bridge enqueued one upload per row.
    #[cfg(test)]
    native_copy_submissions: Cell<u64>,
    /// Test observation of copies that encode directly from the texture into the protocol destination.
    #[cfg(test)]
    direct_copy_submissions: Cell<u64>,
    /// Test-only count of empty submissions used to flush a run of aligned buffer writes.
    #[cfg(test)]
    write_flushes: Cell<u64>,
    /// Test-only count of native command buffers submitted by logical command-buffer replay.
    #[cfg(test)]
    command_submissions: Cell<u64>,
    /// Pre-compiled kernels keyed by the shader id a later `CreateShader { PtxKernel, .. }` uses. Stands
    /// in for a driver's PTX front-end for hand-built [`KernelProgram`]s (the `define_kernel` convenience).
    kernels: HashMap<u32, KernelProgram>,
    /// Optional front-end compiling a driver-forwarded [`KernelDescriptor`] into a [`KernelProgram`], kept
    /// out of this crate so no PTX/CUDA code links here. `Send + Sync` so the executor stays shareable
    /// across threads (a multi-tenant host holds it behind a lock).
    #[allow(clippy::type_complexity)]
    kernel_compiler: Option<Box<dyn Fn(&KernelDescriptor) -> Result<KernelProgram> + Send + Sync>>,
    /// Lazily-built cache of the scaled-blit render pipeline/sampler/layout (`Enc::BlitTexture` has no
    /// native wgpu image blit, so it is implemented by rendering — see [`blit`]). Built on first blit and
    /// reused for the executor's lifetime; `None` until a blit is executed.
    blit: Option<blit::BlitCache>,
    /// Content-dedup caches for `CreateShader` / `CreateRenderPipeline`: an identical source or descriptor
    /// aliases an already-compiled backing (shared `Arc` handle, ~0 incremental residency) instead of
    /// recompiling and re-charging. See [`dedup`].
    dedup: dedup::DedupCaches,
}

impl WgpuExecutor {
    /// Acquire a wgpu device per `cfg` (headless software Vulkan by default) and build the executor with
    /// the capability descriptor it will advertise to a negotiating guest.
    pub fn new(cfg: DeviceConfig) -> Result<Self> {
        Ok(Device::new(cfg)?.executor())
    }

    #[cfg(target_os = "macos")]
    /// Acquire a Metal device and verify that it can import an IOSurface-backed texture before advertising
    /// [`PresentKind::IoSurface`]. Failure is explicit; callers may fall back to [`Self::new`] and SHM.
    pub fn new_iosurface(cfg: DeviceConfig) -> Result<Self> {
        Ok(Device::new_iosurface(cfg)?.executor())
    }

    fn from_device(device: &Device) -> Self {
        let gpu = Arc::clone(&device.gpu);
        let modules = Arc::clone(&device.modules);
        let pipelines = Arc::clone(&device.pipelines);
        #[cfg(target_os = "macos")]
        let iosurface = device.iosurface;

        let native_present = {
            #[cfg(target_os = "macos")]
            {
                iosurface.is_some()
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        };
        let caps = Self::capabilities_for(
            &gpu.info.name,
            native_present,
            gpu.features,
            gpu.downlevel.flags,
            gpu.device.limits().max_buffer_size,
        );
        Self {
            gpu,
            pending_writes: AtomicBool::new(false),
            diagnostic_sentinel_buffer: Cell::new(None),
            diagnostic_sentinel_texture: Cell::new(None),
            diagnostic_sentinel_readback: Cell::new(None),
            diagnostic_sentinel_submits: Cell::new(0),
            diagnostic_sentinel_batch_results: Cell::new(0),
            diagnostic_upload_candidates: Cell::new(16),
            modules,
            module_journal: Vec::new(),
            pipelines,
            pipeline_journal: Vec::new(),
            caps,
            #[cfg(target_os = "macos")]
            iosurface,
            #[cfg(target_os = "macos")]
            presentation_completions: Mutex::new(HashMap::new()),
            #[cfg(target_os = "macos")]
            presentation_journal: Vec::new(),
            #[cfg(target_os = "macos")]
            presentation_retirements: Vec::new(),
            #[cfg(test)]
            completion_waits: Cell::new(0),
            #[cfg(test)]
            native_copy_submissions: Cell::new(0),
            #[cfg(test)]
            direct_copy_submissions: Cell::new(0),
            #[cfg(test)]
            write_flushes: Cell::new(0),
            #[cfg(test)]
            command_submissions: Cell::new(0),
            profile: RefCell::new(None),
            kernels: HashMap::new(),
            kernel_compiler: None,
            blit: None,
            dedup: dedup::DedupCaches::default(),
        }
    }

    /// Wait until all work submitted before this call has completed. Use only when CPU-visible data, an
    /// explicit completion fence, or an API contract requires completion; queue ordering is sufficient for
    /// ordinary render, compute, blit, and transfer dependencies.
    fn wait_for_completion(&self) {
        let diagnostics = hl_log::Logging::global().enabled(
            hl_log::Tags::from(hl_log::tag::PRESENT),
            hl_log::Level::Debug,
        );
        let diagnostic_started = diagnostics.then(std::time::Instant::now);
        let profile_started = self
            .profile
            .borrow()
            .as_ref()
            .map(|_| std::time::Instant::now());
        #[cfg(test)]
        self.completion_waits
            .set(self.completion_waits.get().saturating_add(1));
        self.gpu.device.poll(wgpu::Maintain::Wait);
        if let Some(started) = profile_started {
            if let Some(profile) = self.profile.borrow_mut().as_mut() {
                profile.waits.add(started.elapsed());
            }
        }
        if let Some(started) = diagnostic_started {
            hl_log::hl_debug!(
                hl_log::tag::PRESENT,
                "native_present phase=device_poll wait_us={}",
                started.elapsed().as_micros()
            );
        }
    }

    /// Submit queued host writes once. Calling this at every protocol-batch boundary preserves write-only
    /// batches; real queue submissions earlier in the batch already preserve write-before-GPU-work order.
    fn flush_writes(&self) {
        if self.pending_writes.swap(false, Ordering::AcqRel) {
            self.gpu.queue.submit(None::<wgpu::CommandBuffer>);
            #[cfg(test)]
            self.write_flushes
                .set(self.write_flushes.get().saturating_add(1));
        }
    }

    #[cfg(test)]
    fn completion_wait_count(&self) -> u64 {
        self.completion_waits.get()
    }

    #[cfg(all(test, target_os = "macos"))]
    fn presentation_completion_count(&self) -> usize {
        self.presentation_completions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    #[cfg(test)]
    fn native_copy_submission_count(&self) -> u64 {
        self.native_copy_submissions.get()
    }

    #[cfg(test)]
    fn direct_copy_submission_count(&self) -> u64 {
        self.direct_copy_submissions.get()
    }

    #[cfg(test)]
    fn write_flush_count(&self) -> u64 {
        self.write_flushes.get()
    }

    #[cfg(test)]
    fn command_submission_count(&self) -> u64 {
        self.command_submissions.get()
    }

    pub fn enable_profile(&self) {
        *self.profile.borrow_mut() = Some(Profile::default());
    }

    pub fn profile(&self) -> Option<Profile> {
        self.profile.borrow().clone()
    }

    /// The live wgpu device, for the submit-path helpers that create transient resources.
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.gpu.device
    }

    /// An `Instant` only while profiling is enabled, so an un-profiled hot path pays no clock read.
    pub(crate) fn profile_clock(&self) -> Option<std::time::Instant> {
        self.profile
            .borrow()
            .is_some()
            .then(std::time::Instant::now)
    }

    /// Add `started`'s elapsed time to the metric `pick` selects. `started` is `None` when profiling is off.
    pub(crate) fn profile_record(
        &self,
        pick: fn(&mut Profile) -> &mut Metric,
        started: Option<std::time::Instant>,
    ) {
        if let Some(started) = started {
            if let Some(profile) = self.profile.borrow_mut().as_mut() {
                pick(profile).add(started.elapsed());
            }
        }
    }

    /// The human-readable name of the bound adapter (e.g. `"llvmpipe (LLVM 17.0.6, 128 bits)"`), so a
    /// caller/test can assert it landed on the software Vulkan device.
    pub fn adapter_name(&self) -> &str {
        &self.gpu.info.name
    }

    /// The full [`wgpu::AdapterInfo`] of the bound adapter (backend, device type, driver).
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.gpu.info
    }

    /// Register a pre-compiled [`KernelProgram`] under the shader id a subsequent `CreateShader
    /// { kind: PtxKernel, id, .. }` carries — the hand-built-kernel path the conformance suite uses.
    pub fn define_kernel(&mut self, shader_id: u32, program: KernelProgram) {
        self.kernels.insert(shader_id, program);
    }

    /// Inject the kernel front-end that compiles a driver-forwarded [`KernelDescriptor`] into a
    /// [`KernelProgram`], so a real (non-placeholder) `PtxKernel` payload is compiled on the fly.
    pub fn set_kernel_compiler<F>(&mut self, compiler: F)
    where
        F: Fn(&KernelDescriptor) -> Result<KernelProgram> + Send + Sync + 'static,
    {
        self.kernel_compiler = Some(Box::new(compiler));
    }

    /// The capability descriptor this executor advertises. It accepts SPIR-V/GLSL graphics shaders
    /// (translated to WGSL by naga) AND neutral compute kernels (lowered to WGSL compute) — the union the
    /// wgpu path genuinely executes, a strict superset of the CPU oracle's kernel-only shader surface.
    ///
    /// WGSL is intentionally NOT advertised: the protocol's `CreateShader` derives the payload kind from a
    /// leading magic word ([`ShaderPayloadKind`] has no `Wgsl` variant, and a payload with no known magic
    /// is classified as `Msl`, which this executor rejects), so there is no wire path by which a
    /// guest could hand this backend a WGSL payload it would accept. Advertising it would be a capability
    /// lie (a negotiated-but-unaccepted payload); the honest set is SPIRV | GLSL | KERNEL.
    fn capabilities_for(
        name: &str,
        iosurface: bool,
        features: wgpu::Features,
        downlevel: wgpu::DownlevelFlags,
        device_max_buffer_size: u64,
    ) -> Capabilities {
        let mut present_kinds = vec![PresentKind::Shm];
        if iosurface {
            present_kinds.push(PresentKind::IoSurface);
        }
        Capabilities {
            name: format!("hl-wgpu ({name})"),
            unified_memory: false,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: 8192,
            present_kinds,
            wire_version: WIRE_VERSION,
            command_bits: Capabilities::command_bits(REPLAYED_COMMANDS),
            shader_payloads: shader_payload::SPIRV | shader_payload::GLSL | shader_payload::KERNEL,
            // This executor lowers a real `wgpu::StencilState`, so unlike the CPU oracle (whose shared
            // `DEPTH_FORMATS` is depth-only) it also advertises the combined depth+stencil format that a
            // stencil-testing pipeline/attachment requires.
            texture_formats: TextureFormat::bits(COLOR_FORMATS)
                | TextureFormat::bits(DEPTH_FORMATS)
                | TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8])
                // Integer color formats carry raw integer texels this executor really allocates, uploads,
                // renders to and reads back (see `format_coverage::integer`). The CPU oracle cannot, which
                // is why `INTEGER_FORMATS` is advertised here rather than shared through `COLOR_FORMATS`.
                | TextureFormat::bits(hl_gpu::protocol::model::capability::INTEGER_FORMATS)
                | TextureFormat::bits(hl_gpu::protocol::model::capability::NATIVE_FORMATS)
                | if features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC) {
                    TextureFormat::bits(hl_gpu::protocol::model::capability::BC_FORMATS)
                } else {
                    0
                }
                | if features.contains(wgpu::Features::TEXTURE_COMPRESSION_ETC2) {
                    TextureFormat::bits(hl_gpu::protocol::model::capability::ETC2_FORMATS)
                } else { 0 },
            max_frame_bytes: MAX_FRAME_BYTES,
            // DERIVED FROM THE DEVICE, then clamped to what the rest of the path can honour. A guest asking
            // for more than this is refused at validate before anything is allocated, so the ceiling stays a
            // hard pre-allocation DoS guard; it is simply no longer a per-frame budget wearing a
            // per-buffer's name. See [`MAX_ADVERTISED_BUFFER_BYTES`].
            max_buffer_bytes: device_max_buffer_size.min(MAX_ADVERTISED_BUFFER_BYTES),
            max_bind_groups: 4,
            supports_timeline_fences: false,
            binding_arrays: {
                // Buffer and storage-image arrays are scalarized in the SPIR-V/Naga seam, so they
                // do not require wgpu's native resource-array features on Metal.
                let mut bits =
                    hl_gpu::protocol::model::capability::binding_array::UNIFORM_BUFFER
                        | hl_gpu::protocol::model::capability::binding_array::STORAGE_BUFFER
                        | hl_gpu::protocol::model::capability::binding_array::STORAGE_TEXTURE;
                if features.contains(wgpu::Features::TEXTURE_BINDING_ARRAY) {
                    bits |= hl_gpu::protocol::model::capability::binding_array::SAMPLED_TEXTURE
                        | hl_gpu::protocol::model::capability::binding_array::SAMPLER;
                }
                bits
            },
            non_uniform_binding_arrays:
                hl_gpu::protocol::model::capability::binding_array::STORAGE_BUFFER
                    | hl_gpu::protocol::model::capability::binding_array::STORAGE_TEXTURE
                    | if features.contains(
                        wgpu::Features::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING,
                    ) {
                        hl_gpu::protocol::model::capability::binding_array::SAMPLED_TEXTURE
                    } else {
                        0
                    },
            gpu_features: hl_gpu::protocol::model::capability::gpu_feature::ROBUST_BUFFER_ACCESS
                | if downlevel.contains(wgpu::DownlevelFlags::FRAGMENT_WRITABLE_STORAGE) {
                    hl_gpu::protocol::model::capability::gpu_feature::FRAGMENT_STORES_ATOMICS
                } else {
                    0
                }
                | if downlevel.contains(wgpu::DownlevelFlags::DEPTH_BIAS_CLAMP) {
                    hl_gpu::protocol::model::capability::gpu_feature::DEPTH_BIAS_CLAMP
                } else {
                    0
                }
                | if downlevel.contains(wgpu::DownlevelFlags::CUBE_ARRAY_TEXTURES) {
                    hl_gpu::protocol::model::capability::gpu_feature::IMAGE_CUBE_ARRAY
                } else {
                    0
                }
                | if downlevel.contains(wgpu::DownlevelFlags::INDEPENDENT_BLEND) {
                    hl_gpu::protocol::model::capability::gpu_feature::INDEPENDENT_BLEND
                } else {
                    0
                }
                | if downlevel.contains(wgpu::DownlevelFlags::MULTISAMPLED_SHADING) {
                    hl_gpu::protocol::model::capability::gpu_feature::SAMPLE_RATE_SHADING
                } else {
                    0
                },
        }
    }

    /// Executor-side residency (bytes) of the deduped compiled-shader backings — one unique source's worth
    /// per distinct live source, NOT per alias id. After N `CreateShader`s of the same source this is a
    /// single module's charge, not N. See [`dedup`].
    pub fn shader_backing_resident_bytes(&self) -> u64 {
        self.dedup.shader_resident_bytes()
    }

    /// Executor-local residency of compiled render pipelines with live guest aliases. Cross-batch immutable
    /// artifacts are owned and bounded by the device residency cache.
    pub fn pipeline_backing_resident_bytes(&self) -> u64 {
        self.dedup.pipeline_resident_bytes()
    }

    /// Total deduped backing residency (shaders + render pipelines).
    pub fn dedup_resident_bytes(&self) -> u64 {
        self.dedup.resident_bytes()
    }

    /// Resolve a completed `Present` result to its retained IOSurface image.
    ///
    /// `Ok(None)` is the portable SHM path. The returned lease survives later guest texture destruction.
    #[cfg(target_os = "macos")]
    pub fn iosurface_image(
        &self,
        resources: &hl_gpu::SessionResources,
        presentation: hl_gpu::Presentation,
    ) -> Result<Option<IoSurfaceImage>> {
        let completion = self
            .presentation_completions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(presentation.token.get(), presentation.serial.get()));
        let texture = texture::WgpuTexture::get(resources, presentation.texture.0)?;
        let Some(surface) = texture.iosurface.as_ref() else {
            // A capability quietly NOT taken. The caller reads `Ok(None)` as "no native frame this
            // time" and continues, every layer reports success, and the window shows nothing — the
            // shape that reports a blank result as a working one. Nothing else on the path can name
            // the texture that had no IOSurface backing.
            hl_log::hl_verdict!(
                hl_log::tag::WGPU,
                "presentation.no_iosurface_backing",
                texture = presentation.texture.0,
                token = presentation.token.get(),
                serial = presentation.serial.get();
                "presentation texture {} has no IOSurface backing, so no native frame is published \
                 for token {} serial {} — the frame is silently not presented",
                presentation.texture.0,
                presentation.token.get(),
                presentation.serial.get()
            );
            return Ok(None);
        };
        let completion = completion.ok_or(hl_gpu::GpuError::Invalid(
            "wgpu: IOSurface presentation completion is missing",
        ))?;
        Ok(Some(IoSurfaceImage {
            id: surface.id(),
            width: texture.width,
            height: texture.height,
            surface: surface.as_ref().clone(),
            completion,
        }))
    }

    /// Number of DISTINCT live compiled-shader backings (unique modules currently resident).
    pub fn shader_backing_count(&self) -> usize {
        self.dedup.shader_backing_count()
    }

    /// Number of distinct executor-local compiled-render-pipeline backings with live guest aliases.
    pub fn pipeline_backing_count(&self) -> usize {
        self.dedup.pipeline_backing_count()
    }
}

#[cfg(test)]
mod device_tests {
    use std::sync::Arc;

    use hl_gpu::protocol::model::descriptor::{
        BufferDesc, ColorAttachment, Extent3d, Origin3d, TextureDesc, TextureSubresource,
    };
    use hl_gpu::protocol::model::enums::{
        buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat,
    };
    use hl_gpu::{
        BufferId, Cmd, CommandBuffer, Enc, FenceId, GpuError, GpuExecutor, SessionResources,
    };

    use super::{Device, DeviceConfig, WgpuExecutor, MAX_ADVERTISED_BUFFER_BYTES, MAX_FRAME_BYTES};

    /// The single-allocation ceiling comes from the bound device, not from a constant that happened to be
    /// the frame budget. It is `min(device max_buffer_size, MAX_ADVERTISED_BUFFER_BYTES)` — never larger
    /// than the device serves (no over-advertisement) and never larger than the surrounding residency and
    /// host-staging accounting can honour.
    #[test]
    fn buffer_ceiling_is_derived_from_the_device_and_clamped() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let executor = device.executor();
        let reported = executor.gpu.device.limits().max_buffer_size;

        assert_eq!(
            executor.caps.max_buffer_bytes,
            reported.min(MAX_ADVERTISED_BUFFER_BYTES),
            "advertised {} for a device reporting {reported}",
            executor.caps.max_buffer_bytes,
        );
        assert!(
            executor.caps.max_buffer_bytes <= reported,
            "must never advertise more than the device serves",
        );
    }

    /// The DoS guard survives the raise: the ceiling is finite, far below the absurd request that drove
    /// host RSS to 17.9 GiB (`1 << 38`), and enforced before any allocation — so `CreateBuffer` above it is
    /// refused at validate, not attempted.
    #[test]
    fn buffer_ceiling_stays_finite_and_refuses_before_allocating() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let ceiling = executor.caps.max_buffer_bytes;

        assert!(ceiling > 0 && ceiling < u64::MAX, "finite by construction");
        assert!(ceiling < 1u64 << 38, "far below the DoS request");

        let limits =
            hl_gpu::runtime::model::session::Limits::from_capabilities(executor.caps.clone());
        let absurd = Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 1 << 38,
                usage: buffer_usage::COPY_DST,
                label: String::new(),
            },
        );
        assert!(
            matches!(
                hl_gpu::runtime::service::validate::validate(&limits, 64, &[absurd]),
                Err(GpuError::ResourceLimit("buffer bytes")),
            ),
            "an over-ceiling create must be refused at validate",
        );

        // Nothing reached the executor: its resource table is untouched.
        let mut resources = SessionResources::default();
        assert!(executor.execute(&mut resources, &[]).is_ok());
    }

    /// The frame budget and the buffer ceiling are separate values with separate reasons. The frame budget
    /// must stay under the 512 MiB transport pre-read guard; the buffer ceiling need not, and no longer
    /// does. Raising one must not move the other.
    #[test]
    fn frame_budget_and_buffer_ceiling_are_distinct() {
        assert_eq!(MAX_FRAME_BYTES, 256 << 20);
        assert!(
            MAX_FRAME_BYTES <= hl_gpu::transport::adapter::unix::MAX_FRAME_BYTES as u64,
            "the negotiated frame budget must stay under the transport pre-read guard",
        );
        assert!(
            MAX_ADVERTISED_BUFFER_BYTES > MAX_FRAME_BYTES,
            "a single allocation is not bounded by one frame's wire bytes",
        );
        let caps = WgpuExecutor::capabilities_for(
            "test",
            false,
            wgpu::Features::empty(),
            wgpu::DownlevelFlags::empty(),
            u64::MAX,
        );
        assert_eq!(caps.max_frame_bytes, MAX_FRAME_BYTES);
        assert_eq!(caps.max_buffer_bytes, MAX_ADVERTISED_BUFFER_BYTES);
    }

    #[test]
    fn one_device_creates_resource_isolated_executors() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut first = device.executor();
        let mut second = device.executor();

        assert!(Arc::ptr_eq(&first.gpu, &second.gpu));

        let descriptor = BufferDesc {
            size: 4,
            usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            label: String::new(),
        };
        let mut first_resources = SessionResources::default();
        let mut second_resources = SessionResources::default();
        first
            .execute(
                &mut first_resources,
                &[
                    Cmd::CreateBuffer(1, descriptor.clone()),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![1, 2, 3, 4],
                    },
                ],
            )
            .unwrap();
        second
            .execute(
                &mut second_resources,
                &[
                    Cmd::CreateBuffer(1, descriptor),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![5, 6, 7, 8],
                    },
                ],
            )
            .unwrap();

        assert_eq!(
            first
                .read_buffer(&first_resources, BufferId(1), 0, 4)
                .unwrap(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            second
                .read_buffer(&second_resources, BufferId(1), 0, 4)
                .unwrap(),
            [5, 6, 7, 8]
        );

        first
            .execute(&mut first_resources, &[Cmd::DestroyBuffer(1)])
            .unwrap();
        assert_eq!(
            second
                .read_buffer(&second_resources, BufferId(1), 0, 4)
                .unwrap(),
            [5, 6, 7, 8]
        );
    }

    #[test]
    fn aligned_write_run_flushes_once_and_copy_observes_latest_bytes() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let descriptor = BufferDesc {
            size: 16,
            usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            label: String::new(),
        };

        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(1, descriptor.clone()),
                    Cmd::CreateBuffer(2, descriptor),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![1, 2, 3, 4],
                    },
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 4,
                        data: vec![5, 6, 7, 8],
                    },
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 8,
                        data: vec![9, 10, 11, 12],
                    },
                ],
            )
            .unwrap();
        assert_eq!(executor.write_flush_count(), 1);
        let submissions = executor.command_submission_count();
        let waits = executor.completion_wait_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToBuffer {
                        src: 1,
                        src_offset: 0,
                        dst: 2,
                        dst_offset: 0,
                        size: 12,
                    }],
                    signal: None,
                })],
            )
            .unwrap();

        // A compatible copy stays in the command encoder: no CPU readback, destination queue write, or
        // additional write-only flush. The three source writes above still contributed one flush, not three.
        assert_eq!(executor.write_flush_count(), 1);
        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(
            executor
                .read_buffer(&resources, BufferId(2), 0, 12)
                .unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
    }

    #[test]
    fn ordered_native_buffer_copies_share_one_submission() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let descriptor = BufferDesc {
            size: 8,
            usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            label: String::new(),
        };
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(1, descriptor.clone()),
                    Cmd::CreateBuffer(2, descriptor.clone()),
                    Cmd::CreateBuffer(3, descriptor),
                    Cmd::CreateFence(9),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
                    },
                ],
            )
            .unwrap();
        let submissions = executor.command_submission_count();
        let waits = executor.completion_wait_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToBuffer {
                            src: 1,
                            src_offset: 0,
                            dst: 2,
                            dst_offset: 0,
                            size: 8,
                        },
                        Enc::CopyBufferToBuffer {
                            src: 2,
                            src_offset: 0,
                            dst: 3,
                            dst_offset: 0,
                            size: 8,
                        },
                    ],
                    signal: Some((9, 1)),
                })],
            )
            .unwrap();

        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(
            executor.completion_wait_count(),
            waits,
            "fence enqueue must not synchronously drain the GPU queue"
        );
        executor.wait(&mut resources, FenceId(9), 1).unwrap();
        assert_eq!(executor.completion_wait_count(), waits + 1);
        assert_eq!(
            executor.read_buffer(&resources, BufferId(3), 0, 8).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn ordered_native_texture_copies_share_one_submission() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let texture = TextureDesc {
            width: 2,
            height: 1,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::COPY_SRC | texture_usage::COPY_DST,
            label: String::new(),
        };
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateTexture(1, texture.clone()),
                    Cmd::CreateTexture(2, texture.clone()),
                    Cmd::CreateTexture(3, texture),
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: 8,
                            usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
                    },
                ],
            )
            .unwrap();
        let copy = |src, dst| Enc::CopyTextureToTexture {
            src,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d::default(),
            dst,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d::default(),
            extent: Extent3d {
                width: 2,
                height: 1,
                depth: 1,
            },
        };
        let submissions = executor.command_submission_count();
        let waits = executor.completion_wait_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToTexture {
                            src: 1,
                            src_offset: 0,
                            bytes_per_row: 8,
                            dst: 1,
                            mip: 0,
                            width: 2,
                            height: 1,
                        },
                        copy(1, 2),
                        copy(2, 3),
                    ],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(
            executor.read_texture(&resources, 3).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn aligned_buffer_to_texture_upload_is_native_and_exact() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: 256 * 8,
                            usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![0xff; 256 * 8],
                    },
                    Cmd::CreateTexture(
                        1,
                        TextureDesc {
                            width: 8,
                            height: 8,
                            depth: 1,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::Rgba8Unorm,
                            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();
        let submissions = executor.command_submission_count();
        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 256,
                        dst: 1,
                        mip: 0,
                        width: 8,
                        height: 8,
                    }],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(
            executor.read_texture(&resources, 1).unwrap(),
            vec![0xff; 8 * 8 * 4]
        );
    }

    #[test]
    fn unaligned_texture_pitch_repacks_on_gpu_and_ignores_padding() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let source = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 0xaa, 0xaa, 0xaa, 0xaa, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: source.len() as u64,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: source,
                    },
                    Cmd::CreateTexture(
                        1,
                        TextureDesc {
                            width: 2,
                            height: 2,
                            depth: 1,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::Rgba8Unorm,
                            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();
        let submissions = executor.command_submission_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 12,
                        dst: 1,
                        mip: 0,
                        width: 2,
                        height: 2,
                    }],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(
            executor.read_texture(&resources, 1).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn staged_upload_and_texture_copy_share_exact_order_and_one_submission() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let source = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 0xaa, 0xaa, 0xaa, 0xaa, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let texture = TextureDesc {
            width: 2,
            height: 2,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
            label: String::new(),
        };
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: source.len() as u64,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: source,
                    },
                    Cmd::CreateTexture(1, texture.clone()),
                    Cmd::CreateTexture(2, texture),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();
        let submissions = executor.command_submission_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToTexture {
                            src: 1,
                            src_offset: 0,
                            bytes_per_row: 12,
                            dst: 1,
                            mip: 0,
                            width: 2,
                            height: 2,
                        },
                        Enc::CopyTextureToTexture {
                            src: 1,
                            src_sub: TextureSubresource::base(),
                            src_origin: Origin3d::default(),
                            dst: 2,
                            dst_sub: TextureSubresource::base(),
                            dst_origin: Origin3d::default(),
                            extent: Extent3d {
                                width: 2,
                                height: 2,
                                depth: 1,
                            },
                        },
                    ],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(
            executor.read_texture(&resources, 2).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn staged_multilayer_upload_uses_rows_per_image_without_waiting() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: 8,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
                    },
                    Cmd::CreateTexture(
                        1,
                        TextureDesc {
                            width: 1,
                            height: 1,
                            depth: 2,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::Rgba8Unorm,
                            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();
        let submissions = executor.command_submission_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 1,
                        mip: 0,
                        width: 1,
                        height: 1,
                    }],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(executor.command_submission_count(), submissions + 1);
        assert_eq!(executor.read_texture(&resources, 1).unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn non_four_byte_texture_rows_keep_exact_cpu_fallback() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: 6,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![1, 2, 3, 4, 5, 6],
                    },
                    Cmd::CreateTexture(
                        1,
                        TextureDesc {
                            width: 3,
                            height: 2,
                            depth: 1,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::R8Unorm,
                            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 3,
                        dst: 1,
                        mip: 0,
                        width: 3,
                        height: 2,
                    }],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.completion_wait_count(), waits + 1);
        assert_eq!(
            executor.read_texture(&resources, 1).unwrap(),
            [1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn unaligned_buffer_to_texture_offset_keeps_exact_fallback() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: 9,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                    Cmd::WriteBuffer {
                        id: 1,
                        offset: 0,
                        data: vec![0xaa, 1, 2, 3, 4, 5, 6, 7, 8],
                    },
                    Cmd::CreateTexture(
                        1,
                        TextureDesc {
                            width: 2,
                            height: 1,
                            depth: 1,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::Rgba8Unorm,
                            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();

        executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 1,
                        bytes_per_row: 8,
                        dst: 1,
                        mip: 0,
                        width: 2,
                        height: 1,
                    }],
                    signal: None,
                })],
            )
            .unwrap();

        assert_eq!(executor.completion_wait_count(), waits + 1);
        assert_eq!(
            executor.read_texture(&resources, 1).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn native_buffer_to_texture_rejects_source_overhang_before_submission() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(
                        1,
                        BufferDesc {
                            size: 511,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                    Cmd::CreateTexture(
                        1,
                        TextureDesc {
                            width: 64,
                            height: 2,
                            depth: 1,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::Rgba8Unorm,
                            usage: texture_usage::COPY_DST | texture_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ),
                ],
            )
            .unwrap();
        let waits = executor.completion_wait_count();
        let submissions = executor.command_submission_count();

        let error = executor
            .execute(
                &mut resources,
                &[Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 256,
                        dst: 1,
                        mip: 0,
                        width: 64,
                        height: 2,
                    }],
                    signal: None,
                })],
            )
            .unwrap_err();

        assert!(matches!(error, GpuError::OutOfBounds));
        assert_eq!(executor.completion_wait_count(), waits);
        assert_eq!(executor.command_submission_count(), submissions);
    }

    #[test]
    fn three_ordered_render_passes_share_one_native_submission() {
        let device = Device::new(DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut executor = device.executor();
        let mut resources = SessionResources::default();
        let texture = TextureDesc {
            width: 2,
            height: 1,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            label: String::new(),
        };
        let pass = |target, color| {
            vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: target,
                        load: LoadOp::Clear,
                        clear: color,
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ]
        };
        let mut encoder = pass(1, [1.0, 0.0, 0.0, 1.0]);
        encoder.extend(pass(2, [0.0, 1.0, 0.0, 1.0]));
        encoder.extend(pass(1, [0.0, 0.0, 1.0, 1.0]));
        let submissions = executor.command_submission_count();

        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateTexture(1, texture.clone()),
                    Cmd::CreateTexture(2, texture),
                    Cmd::Submit(CommandBuffer {
                        encoder,
                        signal: None,
                    }),
                ],
            )
            .unwrap();

        assert_eq!(executor.command_submission_count(), submissions + 1);
        let buffer = BufferDesc {
            size: 8,
            usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
            label: String::new(),
        };
        executor
            .execute(
                &mut resources,
                &[
                    Cmd::CreateBuffer(10, buffer.clone()),
                    Cmd::CreateBuffer(11, buffer),
                    Cmd::Submit(CommandBuffer {
                        encoder: vec![
                            Enc::CopyTextureToBuffer {
                                src: 1,
                                mip: 0,
                                width: 2,
                                height: 1,
                                dst: 10,
                                dst_offset: 0,
                                bytes_per_row: 8,
                            },
                            Enc::CopyTextureToBuffer {
                                src: 2,
                                mip: 0,
                                width: 2,
                                height: 1,
                                dst: 11,
                                dst_offset: 0,
                                bytes_per_row: 8,
                            },
                        ],
                        signal: None,
                    }),
                ],
            )
            .unwrap();
        assert_eq!(
            executor
                .read_buffer(&resources, BufferId(10), 0, 8)
                .unwrap(),
            [0, 0, 255, 255, 0, 0, 255, 255]
        );
        assert_eq!(
            executor
                .read_buffer(&resources, BufferId(11), 0, 8)
                .unwrap(),
            [0, 255, 0, 255, 0, 255, 0, 255]
        );
    }
}
