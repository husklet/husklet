//! The `GpuBackend` executor abstraction — the seam between the forwarded hl-GPU IR and a concrete
//! host GPU.
//!
//! One trait, several implementors:
//!   * [`crate::mock::RecordingBackend`] — records the replayed command sequence (tests, this host).
//!   * [`crate::software::SoftwareBackend`] — a real CPU executor (buffers/textures/clears/copies +
//!     readback), the standing correctness fallback; runs headless here.
//!   * `hl_gpu_wgpu::WgpuBackend` — real Metal on an Apple-silicon Mac host, in the SEPARATE
//!     `hl-gpu-wgpu` crate (mac-only; built via `make mac-crates`). It runs on the SAME `MTLDevice` +
//!     `MTLCommandQueue` the display renderer owns (`hl_display::metal::MetalCtx`, constructible via
//!     `MetalCtx::from_device`), so a guest's rendered `MTLTexture`/IOSurface can be composited by
//!     `hl-display` with no cross-device copy (GPU rung 2).
//!   * a future `CudaBackend` — a native Vulkan+CUDA-interop executor on an NVIDIA host.
//!
//! Backends receive **guest-assigned ids** and keep their own id→object map (a [`crate::id::ResourceTable`]
//! per kind is provided for that). The trait is intentionally *not* typed in terms of `ash`/Metal so it
//! stays dependency-free and a future direct-Metal or CUDA-compute backend can implement it without a
//! Vulkan runtime.

use crate::id::*;
use crate::ir::*;
use crate::wire::{Decoder, Encoder};
use crate::{GpuError, Result};

/// Where a presented frame's pixels live, so the HLP layer can attach the right buffer kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentKind {
    /// POSIX-shm region (CPU / software path) — HLP `Buffer(SHM)`.
    Shm,
    /// IOSurface handed over a mach-port (Apple GPU path) — HLP `Buffer(IOSURFACE)`.
    IoSurface,
    /// dma-buf fd (Linux/NVIDIA host GPU path) — HLP `Buffer(DMABUF)`.
    DmaBuf,
}

/// An opaque token identifying the buffer a `present` produced, to be carried in HLP `BUFFER_ATTACH`.
/// `handle` is the shm region id, the IOSurface mach-port name, or the dma-buf fd number depending on
/// `kind`; the real fd/port is passed out-of-band over the HLP socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PresentToken {
    pub surface: u32,
    pub kind: PresentKind,
    pub handle: u64,
    pub width: u32,
    pub height: u32,
    pub format_ok: bool,
}

/// Shader payload kinds a backend can consume. A guest front end (GL/Vulkan/CUDA) negotiates its shader
/// ABI against a backend's advertised set BEFORE advertising a feature to the app, so backend selection
/// can no longer silently change semantics after the guest already promised an API. Bitset (`u32`).
pub mod shader_payload {
    /// SPIR-V words (the Vulkan ABI; naga consumes it host-side).
    pub const SPIRV: u32 = 1 << 0;
    /// GLSL-ES / GLSL source (translated host-side).
    pub const GLSL: u32 = 1 << 1;
    /// Metal Shading Language source/bytes (the bespoke Metal replay's in-guest packing).
    pub const MSL: u32 = 1 << 2;
    /// WGSL source (wgpu-native).
    pub const WGSL: u32 = 1 << 3;
    /// CUDA PTX (compiled to hl-GPU kernel IR).
    pub const PTX: u32 = 1 << 4;
}

/// Build a supported-command bitset from a slice of [`crate::ir::etag`] encoder-op tag numbers.
pub fn command_bits(etags: &[u8]) -> u64 {
    let mut b = 0u64;
    for &t in etags {
        if t < 64 {
            b |= 1u64 << t;
        }
    }
    b
}

/// Build a supported-format bitset (bit = `TextureFormat::to_u32()`).
pub fn format_bits(formats: &[TextureFormat]) -> u32 {
    let mut b = 0u32;
    for f in formats {
        let n = f.to_u32();
        if n < 32 {
            b |= 1u32 << n;
        }
    }
    b
}

/// The color formats every host backend can materialize (the non-depth `TextureFormat`s).
pub const COLOR_FORMATS: &[TextureFormat] = &[
    TextureFormat::Rgba8Unorm,
    TextureFormat::Bgra8Unorm,
    TextureFormat::Rgba8Srgb,
    TextureFormat::Bgra8Srgb,
    TextureFormat::R8Unorm,
    TextureFormat::Rg8Unorm,
    TextureFormat::Rgba16Float,
    TextureFormat::Rgba32Float,
    TextureFormat::R32Float,
];

/// Every encoder-op tag in the current IR (a backend that replays the whole set advertises this).
pub const ALL_COMMANDS: &[u8] = &[
    etag::BEGIN_RENDER_PASS, etag::END_RENDER_PASS, etag::SET_PIPELINE, etag::SET_BIND_GROUP,
    etag::SET_VERTEX_BUFFER, etag::SET_INDEX_BUFFER, etag::SET_VIEWPORT, etag::SET_SCISSOR,
    etag::CLEAR_RECT, etag::DRAW, etag::DRAW_INDEXED, etag::BEGIN_COMPUTE_PASS, etag::END_COMPUTE_PASS,
    etag::DISPATCH, etag::COPY_B2B, etag::COPY_B2T, etag::COPY_T2B, etag::COPY_T2T, etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE,
];

/// Commands implemented by both hardware executors. Now identical to [`ALL_COMMANDS`]: both the Metal
/// backend (`hl-display::metal_backend`) and the wgpu backend (`hl-gpu-wgpu`) implement genuine
/// multisample resolve (`RESOLVE_TEXTURE`) — a per-sample averaging pass into the dest region, proven by
/// their resolve conformance tests (`metal_resolve` / `texture_resolve`) against the software oracle.
pub const HARDWARE_COMMANDS: &[u8] = &[
    etag::BEGIN_RENDER_PASS, etag::END_RENDER_PASS, etag::SET_PIPELINE, etag::SET_BIND_GROUP,
    etag::SET_VERTEX_BUFFER, etag::SET_INDEX_BUFFER, etag::SET_VIEWPORT, etag::SET_SCISSOR,
    etag::CLEAR_RECT, etag::DRAW, etag::DRAW_INDEXED, etag::BEGIN_COMPUTE_PASS, etag::END_COMPUTE_PASS,
    etag::DISPATCH, etag::COPY_B2B, etag::COPY_B2T, etag::COPY_T2B, etag::COPY_T2T, etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE,
];

/// Present-kind bitset used by the serialized handshake (a `Vec<PresentKind>` is not wire-friendly).
mod present_bit {
    pub const SHM: u32 = 1 << 0;
    pub const IOSURFACE: u32 = 1 << 1;
    pub const DMABUF: u32 = 1 << 2;
}

/// A versioned, serialized capability descriptor a backend advertises and a guest negotiates against
/// BEFORE advertising the corresponding API feature to the app. Replaces the previous coarse booleans:
/// it names the exact wire version, encoder-command set, shader payloads, texture formats, present kinds,
/// and negotiated size/limit ceilings, so an incompatible guest/backend pair fails cleanly at negotiation
/// instead of surfacing as a runtime `BadTag`/`Unsupported` after the app already selected the path.
///
/// The legacy boolean/scalar fields (`unified_memory`, `supports_compute`, …) are retained for existing
/// callers; the new negotiation fields are the authoritative machine-checkable descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub name: String,
    /// True on Apple Silicon / integrated GPUs: CPU and GPU share memory, so buffer uploads are
    /// zero-copy (`MTLBuffer(bytesNoCopy)` / pinned host memory). The single biggest CUDA-on-Metal win.
    pub unified_memory: bool,
    pub supports_compute: bool,
    pub supports_graphics: bool,
    pub max_texture_2d: u32,
    /// The present buffer kinds this backend can hand to HLP.
    pub present_kinds: Vec<PresentKind>,

    // --- versioned negotiation descriptor (Phase 1 capability handshake) ---
    /// hl-gpu IR wire version this backend decodes; a guest with a different [`crate::ir::WIRE_VERSION`]
    /// must be rejected before any command flows (never let a stale pair reinterpret a tag).
    pub wire_version: u32,
    /// Bitset of encoder-op tags (`ir::etag`) this backend replays. Bit N = etag N supported.
    pub command_bits: u64,
    /// Bitset of accepted shader payload kinds ([`shader_payload`]).
    pub shader_payloads: u32,
    /// Bitset of supported texture formats (bit = `TextureFormat::to_u32()`).
    pub texture_formats: u32,
    /// Largest single submitted frame (encoded IR byte-stream) the backend will accept.
    pub max_frame_bytes: u64,
    /// Largest single buffer/texture allocation the backend will accept.
    pub max_buffer_bytes: u64,
    /// Maximum bind groups per pipeline layout.
    pub max_bind_groups: u32,
    /// Whether the backend implements a real external timeline-fence primitive (vs. emulating a fence with
    /// submission completion). Advertised truthfully so a guest cannot promise cross-process timeline sync.
    pub supports_timeline_fences: bool,
}

/// A guest's required feature set, checked against a backend's [`Capabilities`] via [`Capabilities::negotiate`]
/// before the guest advertises the matching API feature to the app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureRequest {
    pub wire_version: u32,
    /// Required shader payload bits ([`shader_payload`]).
    pub shader_payloads: u32,
    /// Required encoder-command bits (use [`command_bits`]).
    pub command_bits: u64,
    /// Required texture-format bits (use [`format_bits`]).
    pub texture_formats: u32,
}

impl Capabilities {
    /// True if the backend replays encoder op `etag`.
    pub fn supports_command(&self, etag: u8) -> bool {
        etag < 64 && self.command_bits & (1u64 << etag) != 0
    }
    /// True if the backend accepts shader payload `bit` (a [`shader_payload`] constant).
    pub fn supports_shader_payload(&self, bit: u32) -> bool {
        self.shader_payloads & bit != 0
    }
    /// True if the backend can materialize texture `format`.
    pub fn supports_format(&self, format: TextureFormat) -> bool {
        let n = format.to_u32();
        n < 32 && self.texture_formats & (1u32 << n) != 0
    }

    /// Negotiate a guest's [`FeatureRequest`] against this descriptor. Returns a typed, clean error the
    /// guest can act on (advertise a lower profile / reject the backend) — NOT a runtime `BadTag` after the
    /// app already committed to the path. Every bit the guest requires must be present in the advertisement.
    pub fn negotiate(&self, req: &FeatureRequest) -> Result<()> {
        if req.wire_version != self.wire_version {
            return Err(GpuError::Unsupported("capability: wire version mismatch"));
        }
        if req.shader_payloads & !self.shader_payloads != 0 {
            return Err(GpuError::Unsupported("capability: shader payload not supported"));
        }
        if req.command_bits & !self.command_bits != 0 {
            return Err(GpuError::Unsupported("capability: command tag not supported"));
        }
        if req.texture_formats & !self.texture_formats != 0 {
            return Err(GpuError::Unsupported("capability: texture format not supported"));
        }
        Ok(())
    }

    fn present_bits(&self) -> u32 {
        let mut b = 0u32;
        for k in &self.present_kinds {
            b |= match k {
                PresentKind::Shm => present_bit::SHM,
                PresentKind::IoSurface => present_bit::IOSURFACE,
                PresentKind::DmaBuf => present_bit::DMABUF,
            };
        }
        b
    }

    /// Serialize this descriptor into the connection handshake byte-stream (the guest decodes it with
    /// [`Capabilities::decode`] and negotiates before advertising any API feature).
    pub fn encode(&self, e: &mut Encoder) {
        e.u32(self.wire_version);
        e.str(&self.name);
        e.bool(self.unified_memory);
        e.bool(self.supports_compute);
        e.bool(self.supports_graphics);
        e.bool(self.supports_timeline_fences);
        e.u32(self.max_texture_2d);
        e.u32(self.max_bind_groups);
        e.u64(self.max_frame_bytes);
        e.u64(self.max_buffer_bytes);
        e.u64(self.command_bits);
        e.u32(self.shader_payloads);
        e.u32(self.texture_formats);
        e.u32(self.present_bits());
    }

    /// Serialize to a standalone handshake frame (u32 length + body).
    pub fn to_handshake(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.frame(|inner| self.encode(inner));
        e.into_vec()
    }

    /// Decode a handshake descriptor produced by [`Capabilities::encode`].
    pub fn decode(d: &mut Decoder) -> Result<Capabilities> {
        let wire_version = d.u32()?;
        let name = d.str()?;
        let unified_memory = d.bool()?;
        let supports_compute = d.bool()?;
        let supports_graphics = d.bool()?;
        let supports_timeline_fences = d.bool()?;
        let max_texture_2d = d.u32()?;
        let max_bind_groups = d.u32()?;
        let max_frame_bytes = d.u64()?;
        let max_buffer_bytes = d.u64()?;
        let command_bits = d.u64()?;
        let shader_payloads = d.u32()?;
        let texture_formats = d.u32()?;
        let pbits = d.u32()?;
        let mut present_kinds = Vec::new();
        if pbits & present_bit::SHM != 0 {
            present_kinds.push(PresentKind::Shm);
        }
        if pbits & present_bit::IOSURFACE != 0 {
            present_kinds.push(PresentKind::IoSurface);
        }
        if pbits & present_bit::DMABUF != 0 {
            present_kinds.push(PresentKind::DmaBuf);
        }
        Ok(Capabilities {
            name,
            unified_memory,
            supports_compute,
            supports_graphics,
            max_texture_2d,
            present_kinds,
            wire_version,
            command_bits,
            shader_payloads,
            texture_formats,
            max_frame_bytes,
            max_buffer_bytes,
            max_bind_groups,
            supports_timeline_fences,
        })
    }

    /// Decode a handshake frame (u32 length + body) written by [`Capabilities::to_handshake`].
    pub fn from_handshake(bytes: &[u8]) -> Result<Capabilities> {
        let mut d = Decoder::new(bytes);
        d.frame(Capabilities::decode)
    }
}

/// The host executor. Object-safe (`&mut dyn GpuBackend`), so the replayer drives any backend.
///
/// Default method bodies return `Unsupported` so a partial backend still compiles and a test backend
/// only overrides what it exercises.
pub trait GpuBackend {
    fn capabilities(&self) -> Capabilities;

    // --- resource lifecycle ---
    fn create_buffer(&mut self, id: BufferId, desc: &BufferDesc) -> Result<()>;
    fn destroy_buffer(&mut self, id: BufferId) -> Result<()>;
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> Result<()>;
    /// Read back device memory (CUDA `cudaMemcpyDtoH`; used by tests + the software path).
    fn read_buffer(&mut self, _id: BufferId, _offset: u64, _out: &mut [u8]) -> Result<()> {
        Err(crate::GpuError::Unsupported("read_buffer"))
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()>;
    fn destroy_texture(&mut self, id: TextureId) -> Result<()>;
    fn read_texture(&mut self, _id: TextureId, _out: &mut [u8]) -> Result<()> {
        Err(crate::GpuError::Unsupported("read_texture"))
    }

    fn create_sampler(&mut self, _id: SamplerId, _desc: &SamplerDesc) -> Result<()> {
        Err(crate::GpuError::Unsupported("create_sampler"))
    }
    fn destroy_sampler(&mut self, _id: SamplerId) -> Result<()> {
        Err(crate::GpuError::Unsupported("destroy_sampler"))
    }

    /// Register a shader module. `spirv` is the committed shader ABI; the Metal backend transpiles it
    /// to MSL/AIR (via SPIRV-Cross/naga) and the CUDA/Vulkan backend consumes it natively.
    fn create_shader(
        &mut self,
        id: ShaderId,
        kind: crate::ir::ShaderPayloadKind,
        words: &[u32],
    ) -> Result<()>;
    fn destroy_shader(&mut self, id: ShaderId) -> Result<()>;

    fn create_render_pipeline(&mut self, _id: PipelineId, _desc: &RenderPipelineDesc) -> Result<()> {
        Err(crate::GpuError::Unsupported("create_render_pipeline"))
    }
    fn create_compute_pipeline(&mut self, _id: PipelineId, _desc: &ComputePipelineDesc) -> Result<()> {
        Err(crate::GpuError::Unsupported("create_compute_pipeline"))
    }
    fn destroy_pipeline(&mut self, _id: PipelineId) -> Result<()> {
        Err(crate::GpuError::Unsupported("destroy_pipeline"))
    }

    fn create_bind_group(&mut self, _id: BindGroupId, _desc: &BindGroupDesc) -> Result<()> {
        Err(crate::GpuError::Unsupported("create_bind_group"))
    }
    fn destroy_bind_group(&mut self, _id: BindGroupId) -> Result<()> {
        Err(crate::GpuError::Unsupported("destroy_bind_group"))
    }

    fn create_surface(&mut self, _id: SurfaceId, _desc: &SurfaceDesc) -> Result<()> {
        Err(crate::GpuError::Unsupported("create_surface"))
    }
    fn destroy_surface(&mut self, _id: SurfaceId) -> Result<()> {
        Err(crate::GpuError::Unsupported("destroy_surface"))
    }

    // --- sync ---
    fn create_fence(&mut self, _id: FenceId) -> Result<()> {
        Err(crate::GpuError::Unsupported("create_fence"))
    }
    fn destroy_fence(&mut self, _id: FenceId) -> Result<()> {
        Err(crate::GpuError::Unsupported("destroy_fence"))
    }
    /// Block until fence `id` reaches `value` (guest `cudaStreamSynchronize` / vkWaitSemaphores).
    fn wait_fence(&mut self, _id: FenceId, _value: u64) -> Result<()> {
        Err(crate::GpuError::Unsupported("wait_fence"))
    }

    // --- work ---
    fn submit(&mut self, cb: &CommandBuffer) -> Result<()>;

    // --- present ---
    fn present(&mut self, _surface: SurfaceId, _texture: TextureId) -> Result<PresentToken> {
        Err(crate::GpuError::Unsupported("present"))
    }
}
