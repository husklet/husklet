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

mod bindgroup;
mod blit;
mod buffer;
mod convert;
mod dedup;
mod device;
mod executor;
mod fence;
mod glsl_es;
mod pipeline;
mod present;
mod reflect;
mod sampler;
mod shader;
mod spirv_split;
mod submit;
mod texture;
mod wgsl;

use std::collections::HashMap;

use hl_gpu::protocol::model::capability::{
    command_bits, format_bits, shader_payload, Capabilities, PresentKind, COLOR_FORMATS,
    DEPTH_FORMATS,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::command::{etag, WIRE_VERSION};
use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::Result;

pub use device::{DeviceConfig, Gpu};

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
    etag::COPY_T2T,
    etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE,
    etag::FILL_BUFFER,
    etag::SET_STENCIL_REFERENCE,
];

/// The wgpu-backed [`hl_gpu::GpuExecutor`]. Holds the acquired device/queue, the negotiated capabilities
/// it advertises, and the kernel front-end state (a pre-registered kernel map + optional PTX-descriptor
/// compiler) that resolves a `PtxKernel` shader payload — the same seam the CPU oracle exposes.
pub struct WgpuExecutor {
    gpu: Gpu,
    caps: Capabilities,
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
        let gpu = device::acquire(&cfg)?;
        let caps = Self::capabilities_for(&gpu.info.name);
        Ok(Self {
            gpu,
            caps,
            kernels: HashMap::new(),
            kernel_compiler: None,
            blit: None,
            dedup: dedup::DedupCaches::default(),
        })
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
    /// is classified as `LegacyMsl`, which this executor rejects), so there is no wire path by which a
    /// guest could hand this backend a WGSL payload it would accept. Advertising it would be a capability
    /// lie (a negotiated-but-unaccepted payload); the honest set is SPIRV | GLSL | KERNEL.
    fn capabilities_for(name: &str) -> Capabilities {
        Capabilities {
            name: format!("hl-wgpu ({name})"),
            unified_memory: false,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: 8192,
            present_kinds: vec![PresentKind::Shm],
            wire_version: WIRE_VERSION,
            command_bits: command_bits(REPLAYED_COMMANDS),
            shader_payloads: shader_payload::SPIRV | shader_payload::GLSL | shader_payload::KERNEL,
            // This executor lowers a real `wgpu::StencilState`, so unlike the CPU oracle (whose shared
            // `DEPTH_FORMATS` is depth-only) it also advertises the combined depth+stencil format that a
            // stencil-testing pipeline/attachment requires.
            texture_formats: format_bits(COLOR_FORMATS)
                | format_bits(DEPTH_FORMATS)
                | format_bits(&[TextureFormat::Depth24PlusStencil8]),
            // Per-frame wire-byte ceiling: a hostile-guest DoS guard (one decoded frame can't force an
            // unbounded transient allocation), NOT a correctness bound — the process-wide `GlobalLedger` is
            // the real host-OOM guard across all connections. Sized BROWSER-CLASS at 256 MiB: the old 64 MiB
            // was mis-sized for a browser — Chrome's real frames were seen at 89–168 MB, and even a healthy
            // browser frame runs well past 64 MiB (the extreme end of that range was inflated by the
            // viewport-NACK roll-back replaying a growing working set, fixed at its root by the viewport
            // clamp in `submit.rs`; 256 MiB is comfortable headroom over a legitimate peak). Stays below the
            // 512 MiB coarse transport pre-read guard (`transport::adapter::unix::MAX_FRAME_BYTES`), which
            // refuses an oversized frame before a body byte is read. Finite by construction.
            max_frame_bytes: 256 << 20,
            max_buffer_bytes: 256 << 20,
            max_bind_groups: 4,
            supports_timeline_fences: false,
        }
    }

    /// Executor-side residency (bytes) of the deduped compiled-shader backings — one unique source's worth
    /// per distinct live source, NOT per alias id. After N `CreateShader`s of the same source this is a
    /// single module's charge, not N. See [`dedup`].
    pub fn shader_backing_resident_bytes(&self) -> u64 {
        self.dedup.shader_resident_bytes()
    }

    /// Executor-side residency (bytes) of the deduped compiled-render-pipeline backings — one unique
    /// descriptor's worth per distinct live descriptor, NOT per alias id.
    pub fn pipeline_backing_resident_bytes(&self) -> u64 {
        self.dedup.pipeline_resident_bytes()
    }

    /// Total deduped backing residency (shaders + render pipelines).
    pub fn dedup_resident_bytes(&self) -> u64 {
        self.dedup.resident_bytes()
    }

    /// Number of DISTINCT live compiled-shader backings (unique modules currently resident).
    pub fn shader_backing_count(&self) -> usize {
        self.dedup.shader_backing_count()
    }

    /// Number of DISTINCT live compiled-render-pipeline backings (unique pipelines currently resident).
    pub fn pipeline_backing_count(&self) -> usize {
        self.dedup.pipeline_backing_count()
    }
}
