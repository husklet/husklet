//! Resource descriptors + texture subresource/region selectors + render-pass attachments.
//!
//! Pure value types with their invariants; serialization lives in [`crate::protocol::codec`]. Enum-valued
//! fields (`format`, `dim`, `topology`, …) use the [`super::enums`] wire enums; opaque WebGPU enum values
//! (`VertexFormat`, blend factors, compare functions) are carried as raw `u32` exactly as on the wire.

use super::enums::{AddressMode, Filter, TextureAspect, TextureDim, TextureFormat, Topology};

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

/// One face's stencil test + operation set. `compare` is an opaque WebGPU compare-function value (the same
/// neutral [`super::enums::compare`] numbering [`DepthState::depth_compare`] uses); `fail_op` /
/// `depth_fail_op` / `pass_op` are opaque WebGPU stencil-operation values ([`super::enums::stencil_op`]).
/// The neutral default — `ALWAYS` compare, `KEEP` on every outcome ([`StencilFaceState::DISABLED`]) — maps
/// to `wgpu::StencilFaceState::IGNORE`, so a `DepthState` whose front+back are both `DISABLED` (with the
/// masks the encoder appends) reproduces the prior no-stencil behavior — the wire-back-compat default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StencilFaceState {
    pub compare: u32,
    pub fail_op: u32,
    pub depth_fail_op: u32,
    pub pass_op: u32,
}

impl StencilFaceState {
    /// The inert face: `ALWAYS` compare and `KEEP` on stencil-fail / depth-fail / pass. This is the neutral
    /// wire default; front+back both `DISABLED` leave the stencil test off (`wgpu`'s `IGNORE`).
    pub const DISABLED: StencilFaceState = StencilFaceState {
        compare: super::enums::compare::ALWAYS,
        fail_op: super::enums::stencil_op::KEEP,
        depth_fail_op: super::enums::stencil_op::KEEP,
        pass_op: super::enums::stencil_op::KEEP,
    };
}

impl Default for StencilFaceState {
    fn default() -> Self {
        StencilFaceState::DISABLED
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct DepthState {
    pub format: TextureFormat,
    pub depth_write: bool,
    /// Opaque WebGPU compare-function value.
    pub depth_compare: u32,
    /// Front-face stencil test + ops. Neutral default ([`StencilFaceState::DISABLED`]) = no stencil test.
    pub stencil_front: StencilFaceState,
    /// Back-face stencil test + ops.
    pub stencil_back: StencilFaceState,
    /// Bits of the stored stencil value the compare reads (WebGPU `stencilReadMask`).
    pub stencil_read_mask: u32,
    /// Bits of the stencil value a pass/fail op may write (WebGPU `stencilWriteMask`).
    pub stencil_write_mask: u32,
}

impl DepthState {
    /// A depth-only pipeline state (format + write-enable + compare) with the stencil test disabled — the
    /// pre-stencil shape. The masks are the WebGPU defaults; because front+back are `DISABLED` the stencil
    /// test stays off regardless, so this is byte-for-byte the old behavior.
    pub fn depth_only(format: TextureFormat, depth_write: bool, depth_compare: u32) -> DepthState {
        DepthState {
            format,
            depth_write,
            depth_compare,
            stencil_front: StencilFaceState::DISABLED,
            stencil_back: StencilFaceState::DISABLED,
            stencil_read_mask: 0xffff_ffff,
            stencil_write_mask: 0xffff_ffff,
        }
    }
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
    /// MSAA sample count the pipeline rasterizes at (WebGPU `GPUMultisampleState.count` / Vulkan
    /// `rasterizationSamples`). `1` = single-sampled (the neutral wire default, byte-for-byte the
    /// pre-v8 behavior); `> 1` (e.g. `4`) builds a multisampled pipeline that MUST draw into a color
    /// attachment of the SAME `sample_count` and whose result is later resolved to a single-sample
    /// texture via [`super::command::Enc::ResolveTexture`]. Added at [`super::command::WIRE_VERSION`] 8.
    pub sample_count: u32,
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
    /// HLP surface id this GPU surface presents through.
    pub hlp_surface: u32,
}

// ---------------------------------------------------------------------------------------------------
// texture subresources / regions (texture-to-texture copy + blit)
// ---------------------------------------------------------------------------------------------------

/// A texture subresource selector — the (mip level, array layer, aspect) a copy/blit reads or writes.
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
// render-pass attachments (encoder-level)
// ---------------------------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub struct ColorAttachment {
    pub texture: u32,
    pub load: super::enums::LoadOp,
    pub clear: [f32; 4],
    pub store: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DepthAttachment {
    pub texture: u32,
    pub load: super::enums::LoadOp,
    pub clear_depth: f32,
    /// Stencil clear value, used when `load == Clear` and the attachment format carries a stencil aspect
    /// (`Depth24PlusStencil8`); ignored for a depth-only format. Defaults to `0` — the value the executor
    /// clears the stencil plane to for a pass that marks it (see the executor's `run_render_pass`).
    pub clear_stencil: u32,
}
