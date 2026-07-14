//! The top-level command stream ([`Cmd`]) and the encoder-level (command-buffer) ops ([`Enc`]), plus the
//! stable wire constants: [`WIRE_VERSION`], the [`tag`] command tags, and the [`etag`] encoder-op tags.
//!
//! These are pure data + `const`s — the encode/decode that turns them into bytes lives in
//! [`crate::protocol::codec`]. The tags live here (not in `codec`) because both these command types and
//! the capability descriptor ([`super::capability`]) reference them; keeping them in `model` avoids a
//! `model → codec` dependency inversion.

use super::descriptor::*;
use super::enums::IndexFormat;

// ---------------------------------------------------------------------------------------------------
// encoder-level (command-buffer) ops
// ---------------------------------------------------------------------------------------------------

/// One recorded encoder command inside a [`CommandBuffer`]. Mirrors a WebGPU/Vulkan command encoder.
#[derive(Clone, PartialEq, Debug)]
pub enum Enc {
    BeginRenderPass {
        color: Vec<ColorAttachment>,
        depth: Option<DepthAttachment>,
    },
    EndRenderPass,
    SetPipeline(u32),
    SetBindGroup {
        index: u32,
        group: u32,
    },
    SetVertexBuffer {
        slot: u32,
        buffer: u32,
        offset: u64,
    },
    SetIndexBuffer {
        buffer: u32,
        offset: u64,
        format: IndexFormat,
    },
    SetViewport {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        min_depth: f32,
        max_depth: f32,
    },
    SetScissor {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    },
    ClearRect {
        texture: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        color: [f32; 4],
    },
    Draw {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    DrawIndexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    },
    BeginComputePass,
    EndComputePass,
    Dispatch {
        x: u32,
        y: u32,
        z: u32,
    },
    CopyBufferToBuffer {
        src: u32,
        src_offset: u64,
        dst: u32,
        dst_offset: u64,
        size: u64,
    },
    CopyBufferToTexture {
        src: u32,
        src_offset: u64,
        bytes_per_row: u32,
        dst: u32,
        mip: u32,
        width: u32,
        height: u32,
    },
    CopyTextureToBuffer {
        src: u32,
        mip: u32,
        width: u32,
        height: u32,
        dst: u32,
        dst_offset: u64,
        bytes_per_row: u32,
    },
    /// Exact-size texture→texture copy (no scaling): `extent` texels move from `src`'s
    /// `src_sub`/`src_origin` to `dst`'s `dst_sub`/`dst_origin`. Formats must be copy-compatible (equal
    /// bytes-per-texel). Wire tag 18 (added at [`WIRE_VERSION`] 2).
    CopyTextureToTexture {
        src: u32,
        src_sub: TextureSubresource,
        src_origin: Origin3d,
        dst: u32,
        dst_sub: TextureSubresource,
        dst_origin: Origin3d,
        extent: Extent3d,
    },
    /// Scaled/filtered texture→texture blit: the `src_extent` region of `src` is resampled with `filter`
    /// into the `dst_extent` region of `dst` (equal extents = a straight copy). Wire tag 19 (added at
    /// [`WIRE_VERSION`] 2).
    BlitTexture {
        src: u32,
        src_sub: TextureSubresource,
        src_origin: Origin3d,
        src_extent: Extent3d,
        dst: u32,
        dst_sub: TextureSubresource,
        dst_origin: Origin3d,
        dst_extent: Extent3d,
        filter: super::enums::Filter,
    },
    /// Resolve a multisampled color region into a single-sampled texture. Unlike copy/blit this
    /// averages distinguishable samples and therefore has its own negotiated operation. Wire tag 20
    /// (added at [`WIRE_VERSION`] 4).
    ResolveTexture {
        src: u32,
        src_sub: TextureSubresource,
        src_origin: Origin3d,
        dst: u32,
        dst_sub: TextureSubresource,
        dst_origin: Origin3d,
        extent: Extent3d,
    },
    /// Fill a buffer range `[offset, offset+size)` with a repeating little-endian 4-byte pattern
    /// (`value`), without expanding to a giant `WriteBuffer`. A device-side memset. Wire tag 21 (added
    /// at [`WIRE_VERSION`] 5). `buffer` follows the [`Enc`] convention of a raw `u32` resource id (see the
    /// copy ops); it names a [`super::id::BufferId`].
    FillBuffer {
        buffer: u32,
        offset: u64,
        size: u64,
        value: u32,
    },
}

impl Enc {
    /// Stable negotiated wire tag for this encoder operation.
    pub fn wire_tag(&self) -> u8 {
        match self {
            Self::BeginRenderPass { .. } => etag::BEGIN_RENDER_PASS,
            Self::EndRenderPass => etag::END_RENDER_PASS,
            Self::SetPipeline(_) => etag::SET_PIPELINE,
            Self::SetBindGroup { .. } => etag::SET_BIND_GROUP,
            Self::SetVertexBuffer { .. } => etag::SET_VERTEX_BUFFER,
            Self::SetIndexBuffer { .. } => etag::SET_INDEX_BUFFER,
            Self::SetViewport { .. } => etag::SET_VIEWPORT,
            Self::SetScissor { .. } => etag::SET_SCISSOR,
            Self::ClearRect { .. } => etag::CLEAR_RECT,
            Self::Draw { .. } => etag::DRAW,
            Self::DrawIndexed { .. } => etag::DRAW_INDEXED,
            Self::BeginComputePass => etag::BEGIN_COMPUTE_PASS,
            Self::EndComputePass => etag::END_COMPUTE_PASS,
            Self::Dispatch { .. } => etag::DISPATCH,
            Self::CopyBufferToBuffer { .. } => etag::COPY_B2B,
            Self::CopyBufferToTexture { .. } => etag::COPY_B2T,
            Self::CopyTextureToBuffer { .. } => etag::COPY_T2B,
            Self::CopyTextureToTexture { .. } => etag::COPY_T2T,
            Self::BlitTexture { .. } => etag::BLIT_TEXTURE,
            Self::ResolveTexture { .. } => etag::RESOLVE_TEXTURE,
            Self::FillBuffer { .. } => etag::FILL_BUFFER,
        }
    }
}

/// A recorded command buffer, optionally signalling a fence to `value` on completion.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CommandBuffer {
    pub encoder: Vec<Enc>,
    pub signal: Option<(u32, u64)>, // (FenceId, timeline value)
}

/// Declares the origin and required handling of a shader-word payload.
///
/// `SpirV` is strict: an executor must translate it or return an error — it must never silently
/// substitute a built-in shader. Legacy/demo payloads opt into compatibility handling explicitly, while
/// neutral kernel descriptors remain a separate, self-identifying channel (classified by
/// [`super::kernel::KERNEL_MAGIC`], never a CUDA/PTX constant).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ShaderPayloadKind {
    SpirV = 1,
    LegacyMsl = 2,
    PtxKernel = 3,
    DemoBuiltin = 4,
}

// ---------------------------------------------------------------------------------------------------
// top-level command stream
// ---------------------------------------------------------------------------------------------------

/// One hl-GPU IR command. The guest emits a stream of these; a `CommandSink`/backend replays them.
#[derive(Clone, PartialEq, Debug)]
pub enum Cmd {
    CreateBuffer(u32, BufferDesc),
    DestroyBuffer(u32),
    WriteBuffer { id: u32, offset: u64, data: Vec<u8> },
    CreateTexture(u32, TextureDesc),
    DestroyTexture(u32),
    CreateSampler(u32, SamplerDesc),
    DestroySampler(u32),
    CreateShader { id: u32, kind: ShaderPayloadKind, spirv: Vec<u32> },
    DestroyShader(u32),
    CreateRenderPipeline(u32, RenderPipelineDesc),
    CreateComputePipeline(u32, ComputePipelineDesc),
    DestroyPipeline(u32),
    CreateBindGroup(u32, BindGroupDesc),
    DestroyBindGroup(u32),
    CreateSurface(u32, SurfaceDesc),
    DestroySurface(u32),
    CreateFence(u32),
    DestroyFence(u32),
    Submit(CommandBuffer),
    WaitFence { id: u32, value: u64 },
    Present { surface: u32, texture: u32 },
}

/// hl-GPU IR wire-format version. Bump this whenever a new `Cmd`/`Enc` tag or descriptor field is added
/// so a negotiated handshake can reject a stale guest/backend pair before it interprets a tag it predates.
/// The decoder's hard `BadTag` rejection of any unknown tag is the standing guarantee that a v1 backend
/// can never silently misread a v2 tag: it errors the frame rather than aliasing it onto an older meaning.
///
/// - v1: the original command/encoder set (tags ≤ 21 / etags ≤ 17).
/// - v2: adds texture subresources + `CopyTextureToTexture` (etag 18) and `BlitTexture` (etag 19).
/// - v3: makes every shader payload's origin explicit; strict SPIR-V may not fall back to built-ins.
/// - v4: adds a distinct multisample resolve operation (etag 20).
/// - v5: adds a buffer-fill (device memset) operation, `FillBuffer` (etag 21). Purely additive — no
///   existing message's bytes change; a v4 decoder rejects etag 21 as `BadTag` rather than aliasing it.
pub const WIRE_VERSION: u32 = 5;

// tag constants (stable wire) --------------------------------------------------------------------
/// Top-level [`Cmd`] tag numbers.
pub mod tag {
    pub const CREATE_BUFFER: u8 = 1;
    pub const DESTROY_BUFFER: u8 = 2;
    pub const WRITE_BUFFER: u8 = 3;
    pub const CREATE_TEXTURE: u8 = 4;
    pub const DESTROY_TEXTURE: u8 = 5;
    pub const CREATE_SAMPLER: u8 = 6;
    pub const DESTROY_SAMPLER: u8 = 7;
    pub const CREATE_SHADER: u8 = 8;
    pub const DESTROY_SHADER: u8 = 9;
    pub const CREATE_RENDER_PIPELINE: u8 = 10;
    pub const CREATE_COMPUTE_PIPELINE: u8 = 11;
    pub const DESTROY_PIPELINE: u8 = 12;
    pub const CREATE_BIND_GROUP: u8 = 13;
    pub const DESTROY_BIND_GROUP: u8 = 14;
    pub const CREATE_SURFACE: u8 = 15;
    pub const DESTROY_SURFACE: u8 = 16;
    pub const CREATE_FENCE: u8 = 17;
    pub const DESTROY_FENCE: u8 = 18;
    pub const SUBMIT: u8 = 19;
    pub const WAIT_FENCE: u8 = 20;
    pub const PRESENT: u8 = 21;
}

/// Encoder-op (command-buffer) tag numbers. Public so the capability handshake can build a per-backend
/// supported-command bitset and a guest can negotiate a required command against it.
pub mod etag {
    pub const BEGIN_RENDER_PASS: u8 = 1;
    pub const END_RENDER_PASS: u8 = 2;
    pub const SET_PIPELINE: u8 = 3;
    pub const SET_BIND_GROUP: u8 = 4;
    pub const SET_VERTEX_BUFFER: u8 = 5;
    pub const SET_INDEX_BUFFER: u8 = 6;
    pub const SET_VIEWPORT: u8 = 7;
    pub const DRAW: u8 = 8;
    pub const DRAW_INDEXED: u8 = 9;
    pub const BEGIN_COMPUTE_PASS: u8 = 10;
    pub const END_COMPUTE_PASS: u8 = 11;
    pub const DISPATCH: u8 = 12;
    pub const COPY_B2B: u8 = 13;
    pub const COPY_B2T: u8 = 14;
    pub const COPY_T2B: u8 = 15;
    pub const SET_SCISSOR: u8 = 16;
    pub const CLEAR_RECT: u8 = 17;
    // v2 (WIRE_VERSION 2): texture-to-texture copy + scaled blit. A v1 decoder rejects these as BadTag.
    pub const COPY_T2T: u8 = 18;
    pub const BLIT_TEXTURE: u8 = 19;
    // v4 (WIRE_VERSION 4): distinct multisample resolve.
    pub const RESOLVE_TEXTURE: u8 = 20;
    // v5 (WIRE_VERSION 5): buffer fill (device memset). A v4 decoder rejects this as BadTag.
    pub const FILL_BUFFER: u8 = 21;
}
