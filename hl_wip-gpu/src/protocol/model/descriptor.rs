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
}
