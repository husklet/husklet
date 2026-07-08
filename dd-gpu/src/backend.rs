//! The `GpuBackend` executor abstraction — the seam between the forwarded dd-GPU IR and a concrete
//! host GPU.
//!
//! One trait, several implementors:
//!   * [`crate::mock::RecordingBackend`] — records the replayed command sequence (tests, this host).
//!   * [`crate::software::SoftwareBackend`] — a real CPU executor (buffers/textures/clears/copies +
//!     readback), the standing correctness fallback; runs headless here.
//!   * `MetalBackend` — direct Metal on an Apple-silicon Mac host (`--features metal`, mac-only). It
//!     should be built on the SAME `MTLDevice` + `MTLCommandQueue` the display renderer owns
//!     (`dd_display::metal::MetalCtx`, constructible via `MetalCtx::from_device`), so a guest's rendered
//!     `MTLTexture`/IOSurface can be composited by `dd-display` with no cross-device copy (GPU rung 2).
//!   * `CudaBackend` — a native Vulkan+CUDA-interop executor on an NVIDIA host (`--features cuda`).
//!
//! Backends receive **guest-assigned ids** and keep their own id→object map (a [`crate::id::ResourceTable`]
//! per kind is provided for that). The trait is intentionally *not* typed in terms of `ash`/Metal so it
//! stays dependency-free and a future direct-Metal or CUDA-compute backend can implement it without a
//! Vulkan runtime.

use crate::id::*;
use crate::ir::*;
use crate::Result;

/// Where a presented frame's pixels live, so the DDP layer can attach the right buffer kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentKind {
    /// POSIX-shm region (CPU / software path) — DDP `Buffer(SHM)`.
    Shm,
    /// IOSurface handed over a mach-port (Apple GPU path) — DDP `Buffer(IOSURFACE)`.
    IoSurface,
    /// dma-buf fd (Linux/NVIDIA host GPU path) — DDP `Buffer(DMABUF)`.
    DmaBuf,
}

/// An opaque token identifying the buffer a `present` produced, to be carried in DDP `BUFFER_ATTACH`.
/// `handle` is the shm region id, the IOSurface mach-port name, or the dma-buf fd number depending on
/// `kind`; the real fd/port is passed out-of-band over the DDP socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PresentToken {
    pub surface: u32,
    pub kind: PresentKind,
    pub handle: u64,
    pub width: u32,
    pub height: u32,
    pub format_ok: bool,
}

/// Static capabilities a backend advertises (drives feature gating + the simulated CUDA device props).
#[derive(Clone, Debug)]
pub struct Capabilities {
    pub name: String,
    /// True on Apple Silicon / integrated GPUs: CPU and GPU share memory, so buffer uploads are
    /// zero-copy (`MTLBuffer(bytesNoCopy)` / pinned host memory). The single biggest CUDA-on-Metal win.
    pub unified_memory: bool,
    pub supports_compute: bool,
    pub supports_graphics: bool,
    pub max_texture_2d: u32,
    /// The present buffer kinds this backend can hand to DDP.
    pub present_kinds: Vec<PresentKind>,
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
    fn create_shader(&mut self, id: ShaderId, spirv: &[u32]) -> Result<()>;
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
