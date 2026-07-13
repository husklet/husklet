//! The dd-GPU command IR — the host-GPU-agnostic intermediate the guest producer emits and every
//! [`crate::backend::GpuBackend`] replays.
//!
//! Shape: a compact, explicit, **WebGPU/Vulkan-family** command model (bindless-friendly resource
//! table, explicit pipelines, render + compute passes, explicit sync/present) with **SPIR-V as the
//! shader ABI**. It is deliberately *not* raw Vulkan structs and *not* Metal: it is the normalized
//! subset both a Metal executor and a Vulkan-on-NVIDIA executor implement (see
//! `docs/ideas/RENDERING_GPU_BACKENDS.md` for the level-of-forwarding analysis). Enum numbering for
//! `VertexFormat` / blend factors follows WebGPU where noted; the values are stable wire constants.

use crate::wire::{Decoder, Encoder};
use crate::{GpuError, Result};

// ---------------------------------------------------------------------------------------------------
// small helpers for repr-u32 enums
// ---------------------------------------------------------------------------------------------------

macro_rules! u32_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $val:expr),+ $(,)? } $what:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u32)]
        pub enum $name { $($variant = $val),+ }
        impl $name {
            pub fn to_u32(self) -> u32 { self as u32 }
            pub fn from_u32(v: u32) -> Result<Self> {
                match v { $($val => Ok($name::$variant),)+ _ => Err(GpuError::BadEnum { what: $what, val: v }) }
            }
        }
    };
}

u32_enum!(
    /// Pixel format for textures / render targets / swapchain images.
    TextureFormat {
        Rgba8Unorm = 1, Bgra8Unorm = 2, Rgba8Srgb = 3, Bgra8Srgb = 4,
        R8Unorm = 5, Rg8Unorm = 6, Rgba16Float = 7, Rgba32Float = 8,
        R32Float = 9, Depth32Float = 10, Depth24PlusStencil8 = 11,
    } "TextureFormat"
);

impl TextureFormat {
    /// Bytes per texel for the color formats the software backend can materialize.
    pub fn bytes_per_texel(self) -> Option<usize> {
        Some(match self {
            TextureFormat::R8Unorm => 1,
            TextureFormat::Rg8Unorm => 2,
            TextureFormat::Rgba8Unorm
            | TextureFormat::Bgra8Unorm
            | TextureFormat::Rgba8Srgb
            | TextureFormat::Bgra8Srgb
            | TextureFormat::R32Float => 4,
            TextureFormat::Rgba16Float => 8,
            TextureFormat::Rgba32Float => 16,
            // depth/stencil are not plain-color; software backend won't clear-fill these
            TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8 => return None,
        })
    }
}

u32_enum!(
    /// Texture dimensionality.
    TextureDim { D1 = 1, D2 = 2, D3 = 3, Cube = 4 } "TextureDim"
);

u32_enum!(
    /// Index element size.
    IndexFormat { U16 = 1, U32 = 2 } "IndexFormat"
);

u32_enum!(
    /// Primitive assembly topology (WebGPU numbering).
    Topology { PointList = 0, LineList = 1, LineStrip = 2, TriangleList = 3, TriangleStrip = 4 } "Topology"
);

u32_enum!(
    /// Render-pass attachment load behavior.
    LoadOp { Load = 0, Clear = 1, DontCare = 2 } "LoadOp"
);

u32_enum!(
    /// Texture filtering mode.
    Filter { Nearest = 0, Linear = 1 } "Filter"
);

u32_enum!(
    /// Which plane of a texture a copy/blit or subresource selector addresses. Mirrors wgpu's
    /// `TextureAspect` and the `VkImageAspectFlags` (color/depth/stencil) MoltenVK threads through
    /// `MVKCmdCopyImage`/`MVKCmdBlitImage`. The software oracle only materializes color (`All`).
    TextureAspect { All = 0, DepthOnly = 1, StencilOnly = 2 } "TextureAspect"
);

u32_enum!(
    /// Sampler address (wrap) mode.
    AddressMode { ClampToEdge = 0, Repeat = 1, MirrorRepeat = 2 } "AddressMode"
);

// ---------------------------------------------------------------------------------------------------
// usage bitflags (hand-rolled u32 constants — no `bitflags` dep)
// ---------------------------------------------------------------------------------------------------

/// Buffer usage flags. A `cudaMalloc` allocation maps to `STORAGE | COPY_SRC | COPY_DST`.
pub mod buffer_usage {
    pub const VERTEX: u32 = 1 << 0;
    pub const INDEX: u32 = 1 << 1;
    pub const UNIFORM: u32 = 1 << 2;
    pub const STORAGE: u32 = 1 << 3;
    pub const COPY_SRC: u32 = 1 << 4;
    pub const COPY_DST: u32 = 1 << 5;
    pub const INDIRECT: u32 = 1 << 6;
    /// Host-visible / mappable (unified memory on Apple Silicon; pinned/managed on CUDA).
    pub const MAP: u32 = 1 << 7;
}

/// Texture usage flags.
pub mod texture_usage {
    pub const SAMPLED: u32 = 1 << 0;
    pub const STORAGE: u32 = 1 << 1;
    pub const RENDER_TARGET: u32 = 1 << 2;
    pub const COPY_SRC: u32 = 1 << 3;
    pub const COPY_DST: u32 = 1 << 4;
    /// Presentable to a DDP surface (IOSurface-backed on Mac; dma-buf/VkImage on a CUDA host).
    pub const PRESENT: u32 = 1 << 5;
}

// ---------------------------------------------------------------------------------------------------
// resource descriptors
// ---------------------------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct BufferDesc {
    pub size: u64,
    pub usage: u32,
    /// Optional debug label (cheap, dropped by non-debug backends).
    pub label: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub mip_levels: u32,
    pub sample_count: u32,
    pub dim: TextureDim,
    pub format: TextureFormat,
    pub usage: u32,
    pub label: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct SamplerDesc {
    pub min_filter: Filter,
    pub mag_filter: Filter,
    pub mip_filter: Filter,
    pub address_u: AddressMode,
    pub address_v: AddressMode,
    pub address_w: AddressMode,
}

/// A named shader entry point inside a module.
#[derive(Clone, PartialEq, Debug)]
pub struct ShaderRef {
    pub module: u32, // ShaderId.0
    pub entry: String,
}

/// One vertex attribute. `format`/`offset` follow WebGPU's `GPUVertexAttribute`.
#[derive(Clone, PartialEq, Debug)]
pub struct VertexAttr {
    pub location: u32,
    pub format: u32, // opaque WebGPU VertexFormat enum value
    pub offset: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct VertexLayout {
    pub stride: u32,
    /// 0 = per-vertex, 1 = per-instance.
    pub step_mode: u32,
    pub attrs: Vec<VertexAttr>,
}

/// Fixed-function blend for one color target. Factors/ops are opaque WebGPU enum values.
#[derive(Clone, PartialEq, Debug)]
pub struct BlendState {
    pub src_color: u32,
    pub dst_color: u32,
    pub op_color: u32,
    pub src_alpha: u32,
    pub dst_alpha: u32,
    pub op_alpha: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ColorTargetState {
    pub format: TextureFormat,
    pub blend: Option<BlendState>,
    /// RGBA write mask, low 4 bits.
    pub write_mask: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DepthState {
    pub format: TextureFormat,
    pub depth_write: bool,
    /// Opaque WebGPU compare-function value.
    pub depth_compare: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct RenderPipelineDesc {
    pub vertex: ShaderRef,
    pub fragment: Option<ShaderRef>,
    pub vertex_buffers: Vec<VertexLayout>,
    pub color_targets: Vec<ColorTargetState>,
    pub depth: Option<DepthState>,
    pub topology: Topology,
    /// 0 = none, 1 = front, 2 = back.
    pub cull: u32,
    /// 0 = CCW, 1 = CW.
    pub front_face: u32,
    pub label: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ComputePipelineDesc {
    pub compute: ShaderRef,
    pub label: String,
}

/// A single binding within a bind group.
#[derive(Clone, PartialEq, Debug)]
pub enum BindResource {
    Buffer { id: u32, offset: u64, size: u64 },
    Texture { id: u32 },
    Sampler { id: u32 },
}

#[derive(Clone, PartialEq, Debug)]
pub struct BindEntry {
    pub binding: u32,
    pub resource: BindResource,
}

#[derive(Clone, PartialEq, Debug)]
pub struct BindGroupDesc {
    /// Which pipeline layout set index this group binds to.
    pub set: u32,
    pub entries: Vec<BindEntry>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct SurfaceDesc {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    /// DDP surface id this GPU surface presents through.
    pub ddp_surface: u32,
}

// ---------------------------------------------------------------------------------------------------
// texture subresources / regions (texture-to-texture copy + blit)
// ---------------------------------------------------------------------------------------------------

/// A texture subresource selector — dd's first-class "texture view / subresource" concept: the
/// (mip level, array layer, aspect) a copy/blit reads or writes. Mirrors wgpu's
/// `TexelCopyTextureInfo { mip_level, origin.z (array layer), aspect }` and the `VkImageSubresourceLayers`
/// (`mipLevel` / `baseArrayLayer` / `aspectMask`) MoltenVK maps in `MVKCmdCopyImage`/`MVKCmdBlitImage`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureSubresource {
    pub mip: u32,
    pub layer: u32,
    pub aspect: TextureAspect,
}

impl TextureSubresource {
    /// The base subresource: mip 0, layer 0, whole (color) aspect — the common case shims lower to.
    pub fn base() -> Self {
        TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All }
    }
}

/// A 3D texel origin within a texture subresource (`x`/`y` in-plane; `z` = depth slice for a 3D texture).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Origin3d {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

/// A 3D copy/blit extent in texels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent3d {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

// ---------------------------------------------------------------------------------------------------
// encoder-level (command-buffer) ops
// ---------------------------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct ColorAttachment {
    pub texture: u32,
    pub load: LoadOp,
    pub clear: [f32; 4],
    pub store: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DepthAttachment {
    pub texture: u32,
    pub load: LoadOp,
    pub clear_depth: f32,
}

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
    /// bytes-per-texel). Mirrors Metal's `copyFromTexture:…toTexture:…` blit and `VkCmdCopyImage`
    /// (`MVKCmdCopyImage`). Wire tag 18 (added at [`WIRE_VERSION`] 2).
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
    /// into the `dst_extent` region of `dst` (the scale is `dst_extent / src_extent`; equal extents = a
    /// straight copy). Mirrors `VkCmdBlitImage` (`MVKCmdBlitImage`, which renders a filtered quad when the
    /// extents differ) and a wgpu render-pass sampled draw. Wire tag 19 (added at [`WIRE_VERSION`] 2).
    BlitTexture {
        src: u32,
        src_sub: TextureSubresource,
        src_origin: Origin3d,
        src_extent: Extent3d,
        dst: u32,
        dst_sub: TextureSubresource,
        dst_origin: Origin3d,
        dst_extent: Extent3d,
        filter: Filter,
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
        }
    }
}

/// A recorded command buffer, optionally signalling a fence to `value` on completion.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CommandBuffer {
    pub encoder: Vec<Enc>,
    pub signal: Option<(u32, u64)>, // (FenceId, timeline value)
}

/// Declares the origin and required handling of a shader word payload.
///
/// In particular, `SpirV` is strict: an executor must translate it or return an error.  It must
/// never silently substitute a built-in shader.  Legacy/demo payloads opt into compatibility
/// handling explicitly, while CUDA kernel descriptors remain a separate, typed channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ShaderPayloadKind {
    SpirV = 1,
    LegacyMsl = 2,
    PtxKernel = 3,
    DemoBuiltin = 4,
}

impl ShaderPayloadKind {
    fn decode(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::SpirV),
            2 => Ok(Self::LegacyMsl),
            3 => Ok(Self::PtxKernel),
            4 => Ok(Self::DemoBuiltin),
            _ => Err(GpuError::BadTag(tag.into())),
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// top-level command stream
// ---------------------------------------------------------------------------------------------------

/// One dd-GPU IR command. The guest emits a stream of these; a [`crate::backend::GpuBackend`] replays
/// them via [`crate::replay`].
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

/// dd-GPU IR wire-format version. Bump this whenever a new `Cmd`/`Enc` tag or descriptor field is added
/// so a negotiated handshake can reject a stale guest/backend pair before it interprets a tag it predates
/// (the connection preamble in §3 of `docs/codex-rendering.md`). Until that handshake lands, the decoder's
/// hard `BadTag` rejection of any unknown tag is the standing guarantee that a v1 backend can never
/// silently misread a v2 tag: it errors the frame rather than aliasing it onto an older meaning.
///
/// - v1: the original command/encoder set (tags ≤ 21 / etags ≤ 17).
/// - v2: adds texture subresources + `CopyTextureToTexture` (etag 18) and `BlitTexture` (etag 19).
/// - v3: makes every shader payload's origin explicit; strict SPIR-V may not fall back to built-ins.
pub const WIRE_VERSION: u32 = 3;

// tag constants (stable wire) --------------------------------------------------------------------
pub(crate) mod tag {
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

/// Encoder-op (command-buffer) tag numbers. Public so the capability handshake ([`crate::backend`]) can
/// build a per-backend supported-command bitset and a guest can negotiate a required command against it.
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
}

// ---------------------------------------------------------------------------------------------------
// codec: descriptors
// ---------------------------------------------------------------------------------------------------

fn enc_buffer_desc(e: &mut Encoder, d: &BufferDesc) {
    e.u64(d.size);
    e.u32(d.usage);
    e.str(&d.label);
}
fn dec_buffer_desc(d: &mut Decoder) -> Result<BufferDesc> {
    Ok(BufferDesc {
        size: d.u64()?,
        usage: d.u32()?,
        label: d.str()?,
    })
}

fn enc_texture_desc(e: &mut Encoder, t: &TextureDesc) {
    e.u32(t.width);
    e.u32(t.height);
    e.u32(t.depth);
    e.u32(t.mip_levels);
    e.u32(t.sample_count);
    e.u32(t.dim.to_u32());
    e.u32(t.format.to_u32());
    e.u32(t.usage);
    e.str(&t.label);
}
fn dec_texture_desc(d: &mut Decoder) -> Result<TextureDesc> {
    Ok(TextureDesc {
        width: d.u32()?,
        height: d.u32()?,
        depth: d.u32()?,
        mip_levels: d.u32()?,
        sample_count: d.u32()?,
        dim: TextureDim::from_u32(d.u32()?)?,
        format: TextureFormat::from_u32(d.u32()?)?,
        usage: d.u32()?,
        label: d.str()?,
    })
}

fn enc_sampler_desc(e: &mut Encoder, s: &SamplerDesc) {
    e.u32(s.min_filter.to_u32());
    e.u32(s.mag_filter.to_u32());
    e.u32(s.mip_filter.to_u32());
    e.u32(s.address_u.to_u32());
    e.u32(s.address_v.to_u32());
    e.u32(s.address_w.to_u32());
}
fn dec_sampler_desc(d: &mut Decoder) -> Result<SamplerDesc> {
    Ok(SamplerDesc {
        min_filter: Filter::from_u32(d.u32()?)?,
        mag_filter: Filter::from_u32(d.u32()?)?,
        mip_filter: Filter::from_u32(d.u32()?)?,
        address_u: AddressMode::from_u32(d.u32()?)?,
        address_v: AddressMode::from_u32(d.u32()?)?,
        address_w: AddressMode::from_u32(d.u32()?)?,
    })
}

fn enc_shader_ref(e: &mut Encoder, s: &ShaderRef) {
    e.u32(s.module);
    e.str(&s.entry);
}
fn dec_shader_ref(d: &mut Decoder) -> Result<ShaderRef> {
    Ok(ShaderRef {
        module: d.u32()?,
        entry: d.str()?,
    })
}

fn enc_vertex_layout(e: &mut Encoder, v: &VertexLayout) {
    e.u32(v.stride);
    e.u32(v.step_mode);
    e.u32(v.attrs.len() as u32);
    for a in &v.attrs {
        e.u32(a.location);
        e.u32(a.format);
        e.u32(a.offset);
    }
}
fn dec_vertex_layout(d: &mut Decoder) -> Result<VertexLayout> {
    let stride = d.u32()?;
    let step_mode = d.u32()?;
    let n = d.u32()? as usize;
    // each VertexAttr = 3×u32 = 12 bytes
    let mut attrs = Vec::with_capacity(d.cap_count(n, 12));
    for _ in 0..n {
        attrs.push(VertexAttr {
            location: d.u32()?,
            format: d.u32()?,
            offset: d.u32()?,
        });
    }
    Ok(VertexLayout { stride, step_mode, attrs })
}

fn enc_color_target(e: &mut Encoder, c: &ColorTargetState) {
    e.u32(c.format.to_u32());
    match &c.blend {
        None => e.bool(false),
        Some(b) => {
            e.bool(true);
            e.u32(b.src_color);
            e.u32(b.dst_color);
            e.u32(b.op_color);
            e.u32(b.src_alpha);
            e.u32(b.dst_alpha);
            e.u32(b.op_alpha);
        }
    }
    e.u32(c.write_mask);
}
fn dec_color_target(d: &mut Decoder) -> Result<ColorTargetState> {
    let format = TextureFormat::from_u32(d.u32()?)?;
    let blend = if d.bool()? {
        Some(BlendState {
            src_color: d.u32()?,
            dst_color: d.u32()?,
            op_color: d.u32()?,
            src_alpha: d.u32()?,
            dst_alpha: d.u32()?,
            op_alpha: d.u32()?,
        })
    } else {
        None
    };
    Ok(ColorTargetState {
        format,
        blend,
        write_mask: d.u32()?,
    })
}

fn enc_render_pipeline(e: &mut Encoder, p: &RenderPipelineDesc) {
    enc_shader_ref(e, &p.vertex);
    match &p.fragment {
        None => e.bool(false),
        Some(f) => {
            e.bool(true);
            enc_shader_ref(e, f);
        }
    }
    e.u32(p.vertex_buffers.len() as u32);
    for vb in &p.vertex_buffers {
        enc_vertex_layout(e, vb);
    }
    e.u32(p.color_targets.len() as u32);
    for c in &p.color_targets {
        enc_color_target(e, c);
    }
    match &p.depth {
        None => e.bool(false),
        Some(dp) => {
            e.bool(true);
            e.u32(dp.format.to_u32());
            e.bool(dp.depth_write);
            e.u32(dp.depth_compare);
        }
    }
    e.u32(p.topology.to_u32());
    e.u32(p.cull);
    e.u32(p.front_face);
    e.str(&p.label);
}
fn dec_render_pipeline(d: &mut Decoder) -> Result<RenderPipelineDesc> {
    let vertex = dec_shader_ref(d)?;
    let fragment = if d.bool()? { Some(dec_shader_ref(d)?) } else { None };
    let nvb = d.u32()? as usize;
    // each VertexLayout is at least stride+step_mode+count = 12 bytes
    let mut vertex_buffers = Vec::with_capacity(d.cap_count(nvb, 12));
    for _ in 0..nvb {
        vertex_buffers.push(dec_vertex_layout(d)?);
    }
    let nct = d.u32()? as usize;
    // each ColorTargetState is at least format+blend_flag+write_mask = 9 bytes
    let mut color_targets = Vec::with_capacity(d.cap_count(nct, 9));
    for _ in 0..nct {
        color_targets.push(dec_color_target(d)?);
    }
    let depth = if d.bool()? {
        Some(DepthState {
            format: TextureFormat::from_u32(d.u32()?)?,
            depth_write: d.bool()?,
            depth_compare: d.u32()?,
        })
    } else {
        None
    };
    Ok(RenderPipelineDesc {
        vertex,
        fragment,
        vertex_buffers,
        color_targets,
        depth,
        topology: Topology::from_u32(d.u32()?)?,
        cull: d.u32()?,
        front_face: d.u32()?,
        label: d.str()?,
    })
}

fn enc_bind_group(e: &mut Encoder, b: &BindGroupDesc) {
    e.u32(b.set);
    e.u32(b.entries.len() as u32);
    for en in &b.entries {
        e.u32(en.binding);
        match &en.resource {
            BindResource::Buffer { id, offset, size } => {
                e.u8(0);
                e.u32(*id);
                e.u64(*offset);
                e.u64(*size);
            }
            BindResource::Texture { id } => {
                e.u8(1);
                e.u32(*id);
            }
            BindResource::Sampler { id } => {
                e.u8(2);
                e.u32(*id);
            }
        }
    }
}
fn dec_bind_group(d: &mut Decoder) -> Result<BindGroupDesc> {
    let set = d.u32()?;
    let n = d.u32()? as usize;
    // each BindEntry is at least binding+tag+id = 9 bytes
    let mut entries = Vec::with_capacity(d.cap_count(n, 9));
    for _ in 0..n {
        let binding = d.u32()?;
        let resource = match d.u8()? {
            0 => BindResource::Buffer {
                id: d.u32()?,
                offset: d.u64()?,
                size: d.u64()?,
            },
            1 => BindResource::Texture { id: d.u32()? },
            2 => BindResource::Sampler { id: d.u32()? },
            t => return Err(GpuError::BadEnum { what: "BindResource", val: t as u32 }),
        };
        entries.push(BindEntry { binding, resource });
    }
    Ok(BindGroupDesc { set, entries })
}

// ---------------------------------------------------------------------------------------------------
// codec: encoder ops
// ---------------------------------------------------------------------------------------------------

fn enc_subresource(e: &mut Encoder, s: &TextureSubresource) {
    e.u32(s.mip);
    e.u32(s.layer);
    e.u32(s.aspect.to_u32());
}
fn dec_subresource(d: &mut Decoder) -> Result<TextureSubresource> {
    Ok(TextureSubresource {
        mip: d.u32()?,
        layer: d.u32()?,
        aspect: TextureAspect::from_u32(d.u32()?)?,
    })
}

fn enc_origin(e: &mut Encoder, o: &Origin3d) {
    e.u32(o.x);
    e.u32(o.y);
    e.u32(o.z);
}
fn dec_origin(d: &mut Decoder) -> Result<Origin3d> {
    Ok(Origin3d { x: d.u32()?, y: d.u32()?, z: d.u32()? })
}

fn enc_extent(e: &mut Encoder, x: &Extent3d) {
    e.u32(x.width);
    e.u32(x.height);
    e.u32(x.depth);
}
fn dec_extent(d: &mut Decoder) -> Result<Extent3d> {
    Ok(Extent3d { width: d.u32()?, height: d.u32()?, depth: d.u32()? })
}

fn enc_enc(e: &mut Encoder, op: &Enc) {
    match op {
        Enc::BeginRenderPass { color, depth } => {
            e.u8(etag::BEGIN_RENDER_PASS);
            e.u32(color.len() as u32);
            for c in color {
                e.u32(c.texture);
                e.u32(c.load.to_u32());
                for v in c.clear {
                    e.f32(v);
                }
                e.bool(c.store);
            }
            match depth {
                None => e.bool(false),
                Some(dp) => {
                    e.bool(true);
                    e.u32(dp.texture);
                    e.u32(dp.load.to_u32());
                    e.f32(dp.clear_depth);
                }
            }
        }
        Enc::EndRenderPass => e.u8(etag::END_RENDER_PASS),
        Enc::SetPipeline(p) => {
            e.u8(etag::SET_PIPELINE);
            e.u32(*p);
        }
        Enc::SetBindGroup { index, group } => {
            e.u8(etag::SET_BIND_GROUP);
            e.u32(*index);
            e.u32(*group);
        }
        Enc::SetVertexBuffer { slot, buffer, offset } => {
            e.u8(etag::SET_VERTEX_BUFFER);
            e.u32(*slot);
            e.u32(*buffer);
            e.u64(*offset);
        }
        Enc::SetIndexBuffer { buffer, offset, format } => {
            e.u8(etag::SET_INDEX_BUFFER);
            e.u32(*buffer);
            e.u64(*offset);
            e.u32(format.to_u32());
        }
        Enc::SetViewport { x, y, w, h, min_depth, max_depth } => {
            e.u8(etag::SET_VIEWPORT);
            for v in [*x, *y, *w, *h, *min_depth, *max_depth] {
                e.f32(v);
            }
        }
        Enc::SetScissor { x, y, w, h } => {
            e.u8(etag::SET_SCISSOR);
            e.u32(*x);
            e.u32(*y);
            e.u32(*w);
            e.u32(*h);
        }
        Enc::ClearRect { texture, x, y, w, h, color } => {
            e.u8(etag::CLEAR_RECT);
            e.u32(*texture);
            e.u32(*x);
            e.u32(*y);
            e.u32(*w);
            e.u32(*h);
            for v in color {
                e.f32(*v);
            }
        }
        Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
            e.u8(etag::DRAW);
            e.u32(*vertex_count);
            e.u32(*instance_count);
            e.u32(*first_vertex);
            e.u32(*first_instance);
        }
        Enc::DrawIndexed { index_count, instance_count, first_index, base_vertex, first_instance } => {
            e.u8(etag::DRAW_INDEXED);
            e.u32(*index_count);
            e.u32(*instance_count);
            e.u32(*first_index);
            e.i32(*base_vertex);
            e.u32(*first_instance);
        }
        Enc::BeginComputePass => e.u8(etag::BEGIN_COMPUTE_PASS),
        Enc::EndComputePass => e.u8(etag::END_COMPUTE_PASS),
        Enc::Dispatch { x, y, z } => {
            e.u8(etag::DISPATCH);
            e.u32(*x);
            e.u32(*y);
            e.u32(*z);
        }
        Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
            e.u8(etag::COPY_B2B);
            e.u32(*src);
            e.u64(*src_offset);
            e.u32(*dst);
            e.u64(*dst_offset);
            e.u64(*size);
        }
        Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, mip, width, height } => {
            e.u8(etag::COPY_B2T);
            e.u32(*src);
            e.u64(*src_offset);
            e.u32(*bytes_per_row);
            e.u32(*dst);
            e.u32(*mip);
            e.u32(*width);
            e.u32(*height);
        }
        Enc::CopyTextureToBuffer { src, mip, width, height, dst, dst_offset, bytes_per_row } => {
            e.u8(etag::COPY_T2B);
            e.u32(*src);
            e.u32(*mip);
            e.u32(*width);
            e.u32(*height);
            e.u32(*dst);
            e.u64(*dst_offset);
            e.u32(*bytes_per_row);
        }
        Enc::CopyTextureToTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
            e.u8(etag::COPY_T2T);
            e.u32(*src);
            enc_subresource(e, src_sub);
            enc_origin(e, src_origin);
            e.u32(*dst);
            enc_subresource(e, dst_sub);
            enc_origin(e, dst_origin);
            enc_extent(e, extent);
        }
        Enc::BlitTexture {
            src,
            src_sub,
            src_origin,
            src_extent,
            dst,
            dst_sub,
            dst_origin,
            dst_extent,
            filter,
        } => {
            e.u8(etag::BLIT_TEXTURE);
            e.u32(*src);
            enc_subresource(e, src_sub);
            enc_origin(e, src_origin);
            enc_extent(e, src_extent);
            e.u32(*dst);
            enc_subresource(e, dst_sub);
            enc_origin(e, dst_origin);
            enc_extent(e, dst_extent);
            e.u32(filter.to_u32());
        }
    }
}

fn dec_enc(d: &mut Decoder) -> Result<Enc> {
    Ok(match d.u8()? {
        etag::BEGIN_RENDER_PASS => {
            let n = d.u32()? as usize;
            // each ColorAttachment = texture+load+clear[4]+store = 25 bytes
            let mut color = Vec::with_capacity(d.cap_count(n, 25));
            for _ in 0..n {
                let texture = d.u32()?;
                let load = LoadOp::from_u32(d.u32()?)?;
                let clear = [
                    d.f32_finite("color attachment clear r")?,
                    d.f32_finite("color attachment clear g")?,
                    d.f32_finite("color attachment clear b")?,
                    d.f32_finite("color attachment clear a")?,
                ];
                let store = d.bool()?;
                color.push(ColorAttachment { texture, load, clear, store });
            }
            let depth = if d.bool()? {
                Some(DepthAttachment {
                    texture: d.u32()?,
                    load: LoadOp::from_u32(d.u32()?)?,
                    clear_depth: d.f32_finite("depth attachment clear")?,
                })
            } else {
                None
            };
            Enc::BeginRenderPass { color, depth }
        }
        etag::END_RENDER_PASS => Enc::EndRenderPass,
        etag::SET_PIPELINE => Enc::SetPipeline(d.u32()?),
        etag::SET_BIND_GROUP => Enc::SetBindGroup { index: d.u32()?, group: d.u32()? },
        etag::SET_VERTEX_BUFFER => Enc::SetVertexBuffer {
            slot: d.u32()?,
            buffer: d.u32()?,
            offset: d.u64()?,
        },
        etag::SET_INDEX_BUFFER => Enc::SetIndexBuffer {
            buffer: d.u32()?,
            offset: d.u64()?,
            format: IndexFormat::from_u32(d.u32()?)?,
        },
        etag::SET_VIEWPORT => Enc::SetViewport {
            x: d.f32_finite("viewport x")?,
            y: d.f32_finite("viewport y")?,
            w: d.f32_finite("viewport w")?,
            h: d.f32_finite("viewport h")?,
            min_depth: d.f32_finite("viewport min_depth")?,
            max_depth: d.f32_finite("viewport max_depth")?,
        },
        etag::SET_SCISSOR => Enc::SetScissor {
            x: d.u32()?,
            y: d.u32()?,
            w: d.u32()?,
            h: d.u32()?,
        },
        etag::CLEAR_RECT => Enc::ClearRect {
            texture: d.u32()?,
            x: d.u32()?,
            y: d.u32()?,
            w: d.u32()?,
            h: d.u32()?,
            color: [
                d.f32_finite("clear-rect r")?,
                d.f32_finite("clear-rect g")?,
                d.f32_finite("clear-rect b")?,
                d.f32_finite("clear-rect a")?,
            ],
        },
        etag::DRAW => Enc::Draw {
            vertex_count: d.u32()?,
            instance_count: d.u32()?,
            first_vertex: d.u32()?,
            first_instance: d.u32()?,
        },
        etag::DRAW_INDEXED => Enc::DrawIndexed {
            index_count: d.u32()?,
            instance_count: d.u32()?,
            first_index: d.u32()?,
            base_vertex: d.i32()?,
            first_instance: d.u32()?,
        },
        etag::BEGIN_COMPUTE_PASS => Enc::BeginComputePass,
        etag::END_COMPUTE_PASS => Enc::EndComputePass,
        etag::DISPATCH => Enc::Dispatch {
            x: d.u32()?,
            y: d.u32()?,
            z: d.u32()?,
        },
        etag::COPY_B2B => Enc::CopyBufferToBuffer {
            src: d.u32()?,
            src_offset: d.u64()?,
            dst: d.u32()?,
            dst_offset: d.u64()?,
            size: d.u64()?,
        },
        etag::COPY_B2T => Enc::CopyBufferToTexture {
            src: d.u32()?,
            src_offset: d.u64()?,
            bytes_per_row: d.u32()?,
            dst: d.u32()?,
            mip: d.u32()?,
            width: d.u32()?,
            height: d.u32()?,
        },
        etag::COPY_T2B => Enc::CopyTextureToBuffer {
            src: d.u32()?,
            mip: d.u32()?,
            width: d.u32()?,
            height: d.u32()?,
            dst: d.u32()?,
            dst_offset: d.u64()?,
            bytes_per_row: d.u32()?,
        },
        etag::COPY_T2T => Enc::CopyTextureToTexture {
            src: d.u32()?,
            src_sub: dec_subresource(d)?,
            src_origin: dec_origin(d)?,
            dst: d.u32()?,
            dst_sub: dec_subresource(d)?,
            dst_origin: dec_origin(d)?,
            extent: dec_extent(d)?,
        },
        etag::BLIT_TEXTURE => Enc::BlitTexture {
            src: d.u32()?,
            src_sub: dec_subresource(d)?,
            src_origin: dec_origin(d)?,
            src_extent: dec_extent(d)?,
            dst: d.u32()?,
            dst_sub: dec_subresource(d)?,
            dst_origin: dec_origin(d)?,
            dst_extent: dec_extent(d)?,
            filter: Filter::from_u32(d.u32()?)?,
        },
        t => return Err(GpuError::BadTag(t as u32)),
    })
}

fn enc_command_buffer(e: &mut Encoder, cb: &CommandBuffer) {
    e.u32(cb.encoder.len() as u32);
    for op in &cb.encoder {
        enc_enc(e, op);
    }
    match cb.signal {
        None => e.bool(false),
        Some((f, v)) => {
            e.bool(true);
            e.u32(f);
            e.u64(v);
        }
    }
}
fn dec_command_buffer(d: &mut Decoder) -> Result<CommandBuffer> {
    let n = d.u32()? as usize;
    // each encoder op is at least a 1-byte tag
    let mut encoder = Vec::with_capacity(d.cap_count(n, 1));
    for i in 0..n {
        let pos = d.pos();
        let tag = d.peek_u8();
        match dec_enc(d) {
            Ok(op) => encoder.push(op),
            Err(e) => {
                return Err(GpuError::Decode(format!(
                    "submit encoder op {i}/{n} at byte {pos}/{} tag {:?} remaining {}: {e}",
                    d.len(),
                    tag,
                    d.remaining()
                )));
            }
        }
    }
    let signal = if d.bool()? { Some((d.u32()?, d.u64()?)) } else { None };
    Ok(CommandBuffer { encoder, signal })
}

// ---------------------------------------------------------------------------------------------------
// codec: top-level Cmd
// ---------------------------------------------------------------------------------------------------

impl Cmd {
    /// Encode this command (tag + body) into `e`. No length prefix — see [`Cmd::frame`] for that.
    pub fn encode(&self, e: &mut Encoder) {
        match self {
            Cmd::CreateBuffer(id, d) => {
                e.u8(tag::CREATE_BUFFER);
                e.u32(*id);
                enc_buffer_desc(e, d);
            }
            Cmd::DestroyBuffer(id) => {
                e.u8(tag::DESTROY_BUFFER);
                e.u32(*id);
            }
            Cmd::WriteBuffer { id, offset, data } => {
                e.u8(tag::WRITE_BUFFER);
                e.u32(*id);
                e.u64(*offset);
                e.bytes(data);
            }
            Cmd::CreateTexture(id, d) => {
                e.u8(tag::CREATE_TEXTURE);
                e.u32(*id);
                enc_texture_desc(e, d);
            }
            Cmd::DestroyTexture(id) => {
                e.u8(tag::DESTROY_TEXTURE);
                e.u32(*id);
            }
            Cmd::CreateSampler(id, d) => {
                e.u8(tag::CREATE_SAMPLER);
                e.u32(*id);
                enc_sampler_desc(e, d);
            }
            Cmd::DestroySampler(id) => {
                e.u8(tag::DESTROY_SAMPLER);
                e.u32(*id);
            }
            Cmd::CreateShader { id, kind, spirv } => {
                e.u8(tag::CREATE_SHADER);
                e.u32(*id);
                e.u8(*kind as u8);
                e.words(spirv);
            }
            Cmd::DestroyShader(id) => {
                e.u8(tag::DESTROY_SHADER);
                e.u32(*id);
            }
            Cmd::CreateRenderPipeline(id, d) => {
                e.u8(tag::CREATE_RENDER_PIPELINE);
                e.u32(*id);
                enc_render_pipeline(e, d);
            }
            Cmd::CreateComputePipeline(id, d) => {
                e.u8(tag::CREATE_COMPUTE_PIPELINE);
                e.u32(*id);
                enc_shader_ref(e, &d.compute);
                e.str(&d.label);
            }
            Cmd::DestroyPipeline(id) => {
                e.u8(tag::DESTROY_PIPELINE);
                e.u32(*id);
            }
            Cmd::CreateBindGroup(id, d) => {
                e.u8(tag::CREATE_BIND_GROUP);
                e.u32(*id);
                enc_bind_group(e, d);
            }
            Cmd::DestroyBindGroup(id) => {
                e.u8(tag::DESTROY_BIND_GROUP);
                e.u32(*id);
            }
            Cmd::CreateSurface(id, d) => {
                e.u8(tag::CREATE_SURFACE);
                e.u32(*id);
                e.u32(d.width);
                e.u32(d.height);
                e.u32(d.format.to_u32());
                e.u32(d.ddp_surface);
            }
            Cmd::DestroySurface(id) => {
                e.u8(tag::DESTROY_SURFACE);
                e.u32(*id);
            }
            Cmd::CreateFence(id) => {
                e.u8(tag::CREATE_FENCE);
                e.u32(*id);
            }
            Cmd::DestroyFence(id) => {
                e.u8(tag::DESTROY_FENCE);
                e.u32(*id);
            }
            Cmd::Submit(cb) => {
                e.u8(tag::SUBMIT);
                enc_command_buffer(e, cb);
            }
            Cmd::WaitFence { id, value } => {
                e.u8(tag::WAIT_FENCE);
                e.u32(*id);
                e.u64(*value);
            }
            Cmd::Present { surface, texture } => {
                e.u8(tag::PRESENT);
                e.u32(*surface);
                e.u32(*texture);
            }
        }
    }

    /// Decode one command (tag + body) from `d`.
    pub fn decode(d: &mut Decoder) -> Result<Cmd> {
        Ok(match d.u8()? {
            tag::CREATE_BUFFER => Cmd::CreateBuffer(d.u32()?, dec_buffer_desc(d)?),
            tag::DESTROY_BUFFER => Cmd::DestroyBuffer(d.u32()?),
            tag::WRITE_BUFFER => Cmd::WriteBuffer {
                id: d.u32()?,
                offset: d.u64()?,
                data: d.bytes()?.to_vec(),
            },
            tag::CREATE_TEXTURE => Cmd::CreateTexture(d.u32()?, dec_texture_desc(d)?),
            tag::DESTROY_TEXTURE => Cmd::DestroyTexture(d.u32()?),
            tag::CREATE_SAMPLER => Cmd::CreateSampler(d.u32()?, dec_sampler_desc(d)?),
            tag::DESTROY_SAMPLER => Cmd::DestroySampler(d.u32()?),
            tag::CREATE_SHADER => Cmd::CreateShader {
                id: d.u32()?,
                kind: ShaderPayloadKind::decode(d.u8()?)?,
                spirv: d.words()?,
            },
            tag::DESTROY_SHADER => Cmd::DestroyShader(d.u32()?),
            tag::CREATE_RENDER_PIPELINE => Cmd::CreateRenderPipeline(d.u32()?, dec_render_pipeline(d)?),
            tag::CREATE_COMPUTE_PIPELINE => {
                let id = d.u32()?;
                let compute = dec_shader_ref(d)?;
                let label = d.str()?;
                Cmd::CreateComputePipeline(id, ComputePipelineDesc { compute, label })
            }
            tag::DESTROY_PIPELINE => Cmd::DestroyPipeline(d.u32()?),
            tag::CREATE_BIND_GROUP => Cmd::CreateBindGroup(d.u32()?, dec_bind_group(d)?),
            tag::DESTROY_BIND_GROUP => Cmd::DestroyBindGroup(d.u32()?),
            tag::CREATE_SURFACE => {
                let id = d.u32()?;
                Cmd::CreateSurface(
                    id,
                    SurfaceDesc {
                        width: d.u32()?,
                        height: d.u32()?,
                        format: TextureFormat::from_u32(d.u32()?)?,
                        ddp_surface: d.u32()?,
                    },
                )
            }
            tag::DESTROY_SURFACE => Cmd::DestroySurface(d.u32()?),
            tag::CREATE_FENCE => Cmd::CreateFence(d.u32()?),
            tag::DESTROY_FENCE => Cmd::DestroyFence(d.u32()?),
            tag::SUBMIT => Cmd::Submit(dec_command_buffer(d)?),
            tag::WAIT_FENCE => Cmd::WaitFence {
                id: d.u32()?,
                value: d.u64()?,
            },
            tag::PRESENT => Cmd::Present {
                surface: d.u32()?,
                texture: d.u32()?,
            },
            t => return Err(GpuError::BadTag(t as u32)),
        })
    }

    /// Encode as a self-delimiting frame (u32 length + body) for the command ring.
    pub fn frame(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.frame(|inner| self.encode(inner));
        e.into_vec()
    }

    /// Decode one length-prefixed command frame (as produced by [`Cmd::frame`]). Rejects a frame body
    /// that carries extra bytes after the command — a malformed frame is a decode error, not an
    /// accepted command with a silently-discarded tail.
    pub fn decode_frame(d: &mut Decoder) -> Result<Cmd> {
        d.frame(Cmd::decode)
    }
}

/// Encode a whole command stream (tag+body, back to back, no per-message length prefix).
pub fn encode_stream(cmds: &[Cmd]) -> Vec<u8> {
    let mut e = Encoder::new();
    for c in cmds {
        c.encode(&mut e);
    }
    e.into_vec()
}

/// Decode a whole command stream produced by [`encode_stream`] until the input is exhausted.
pub fn decode_stream(bytes: &[u8]) -> Result<Vec<Cmd>> {
    let mut d = Decoder::new(bytes);
    let mut out = Vec::new();
    while !d.is_empty() {
        let idx = out.len();
        let pos = d.pos();
        let tag = d.peek_u8();
        match Cmd::decode(&mut d) {
            Ok(cmd) => out.push(cmd),
            Err(e) => {
                return Err(GpuError::Decode(format!(
                    "command {idx} at byte {pos}/{} tag {:?} remaining {}: {e}",
                    d.len(),
                    tag,
                    d.remaining()
                )));
            }
        }
    }
    Ok(out)
}
