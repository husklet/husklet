//! Resource descriptors + texture subresource/region selectors + render-pass attachments.
//!
//! Pure value types with their invariants; serialization lives in [`crate::protocol::codec`]. Enum-valued
//! fields (`format`, `dim`, `topology`, …) use the [`super::enums`] wire enums. The fields typed as raw
//! `u32` (vertex-attribute formats, blend factors/ops, compare and stencil functions) carry this
//! PROTOCOL's own neutral numbering — NOT a backend enum. They are not WebGPU `VertexFormat` /
//! `GPUBlendFactor` / `GPUCompareFunction` values and not Vulkan `VkFormat` values; forwarding a foreign
//! enum into one of them is a wire violation the executor decodes as a different value entirely.
//!
//! Two cohesive groups live in submodules and are re-exported here, so `descriptor::*` names them all:
//! [`binding`] (pipeline layout + bind groups) and [`surface`] (presentation identity + surface
//! descriptor).

mod binding;
mod surface;

pub use binding::{
    BindEntry, BindGroupDesc, BindResource, BufferBinding, PipelineBinding, PipelineBindingKind,
    PipelineLayout,
};
pub use surface::{FrameSerial, SurfaceDesc, SurfaceToken};

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

/// A typed view into an existing texture's mip and layer range.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextureViewDesc {
    pub texture: u32,
    pub dim: TextureDim,
    pub format: TextureFormat,
    pub aspect: TextureAspect,
    pub base_mip: u32,
    pub mip_count: u32,
    pub base_layer: u32,
    pub layer_count: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct SamplerDesc {
    pub min_filter: Filter,
    pub mag_filter: Filter,
    pub mip_filter: Filter,
    pub address_u: AddressMode,
    pub address_v: AddressMode,
    pub address_w: AddressMode,
    pub lod_min_clamp: f32,
    pub lod_max_clamp: f32,
    /// Neutral [`super::enums::compare`] value. `None` creates a non-comparison sampler.
    pub compare: Option<u32>,
}

impl Default for SamplerDesc {
    fn default() -> Self {
        Self {
            min_filter: Filter::Nearest,
            mag_filter: Filter::Nearest,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            address_w: AddressMode::ClampToEdge,
            lod_min_clamp: 0.0,
            lod_max_clamp: 32.0,
            compare: None,
        }
    }
}

/// A named shader entry point inside a module.
#[derive(Clone, PartialEq, Debug)]
pub struct ShaderRef {
    pub module: u32, // ShaderId.0
    pub entry: String,
}

/// One vertex attribute. `location`/`offset` follow WebGPU's `GPUVertexAttribute`; `format` does NOT —
/// see the field.
#[derive(Clone, PartialEq, Debug)]
pub struct VertexAttr {
    pub location: u32,
    /// PACKED attribute description, NOT a format enum of any API:
    /// `comps | (kind << 8) | (normalized << 16) | (integer << 17)`, where `comps` is 1..=4 and `kind` is
    /// 0=f32 1=u8 2=i8 3=u16 4=i16 5=u32 6=i32 7=f16 8=rgb10a2 (the GL driver's `vertex_format_wire`).
    /// Writing a `wgpu::VertexFormat` or a `VkFormat` here is a wire violation: the executor reads the low
    /// byte as a component count, so `VK_FORMAT_R32G32B32_SFLOAT` (106) becomes a 106-component attribute
    /// and the pipeline is refused. Lower foreign formats into this packing at the guest boundary.
    pub format: u32,
    pub offset: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct VertexLayout {
    pub stride: u32,
    /// 0 = per-vertex, 1 = per-instance.
    pub step_mode: u32,
    pub attrs: Vec<VertexAttr>,
}

impl VertexLayout {
    /// A slot the guest declared no binding for.
    ///
    /// A slot index in this protocol is the source API's own binding number, not a position in a list
    /// that happened to be built in declaration order — so a guest binding only slot 1 leaves slot 0
    /// with nothing in it. This is that hole, stated rather than closed by shifting slot 1 down, which
    /// would put every attribute on the wrong buffer.
    pub fn unused() -> Self {
        Self { stride: 0, step_mode: 0, attrs: Vec::new() }
    }
}

/// Fixed-function blend for one color target. Factors are [`super::enums::blend_factor`] codes and ops are
/// [`super::enums::blend_op`] codes — this protocol's own neutral numbering, not `GPUBlendFactor` /
/// `GPUBlendOperation` and not `VkBlendFactor` / `VkBlendOp`.
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

/// One face's stencil test + operation set. `compare` is a neutral [`super::enums::compare`] code (the
/// same numbering [`DepthState::depth_compare`] uses — `VkCompareOp` ordering, NOT WebGPU's 1-based
/// `GPUCompareFunction`); `fail_op` / `depth_fail_op` / `pass_op` are [`super::enums::stencil_op`] codes.
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
    /// Neutral [`super::enums::compare`] code (`VkCompareOp` ordering: `NEVER` = 0 … `ALWAYS` = 7). NOT
    /// WebGPU's 1-based `GPUCompareFunction`: writing that numbering here shifts every comparison by one.
    pub depth_compare: u32,
    /// Front-face stencil test + ops. Neutral default ([`StencilFaceState::DISABLED`]) = no stencil test.
    pub stencil_front: StencilFaceState,
    /// Back-face stencil test + ops.
    pub stencil_back: StencilFaceState,
    /// Bits of the stored stencil value the compare reads (WebGPU `stencilReadMask`). 32-bit on the wire so
    /// a guest passes an API mask through unchanged, but only the LOW 8 BITS are meaningful — this IR's only
    /// stencil format has an 8-bit plane, so an executor truncates to `& 0xff` rather than rejecting.
    pub stencil_read_mask: u32,
    /// Bits of the stencil value a pass/fail op may write (WebGPU `stencilWriteMask`); low 8 bits only.
    pub stencil_write_mask: u32,
    /// Fixed depth bias. `constant` uses wgpu's integer depth-bias units; `slope_scale` scales the maximum
    /// depth slope; `clamp` limits the resulting bias when the host supports depth-bias clamping.
    pub bias_constant: i32,
    pub bias_slope_scale: f32,
    pub bias_clamp: f32,
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
            bias_constant: 0,
            bias_slope_scale: 0.0,
            bias_clamp: 0.0,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RenderMultisample {
    pub mask: u64,
    /// Force full per-sample fragment execution. This safely satisfies any Vulkan `minSampleShading`
    /// request because full rate is at least the requested minimum.
    pub sample_shading: bool,
}

impl Default for RenderMultisample {
    fn default() -> Self {
        Self {
            mask: u64::MAX,
            sample_shading: false,
        }
    }
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
        TextureSubresource {
            mip: 0,
            layer: 0,
            aspect: TextureAspect::All,
        }
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

/// Per-axis mirroring of a blit: the destination row/column order is reversed relative to the source.
///
/// Origin and extent stay unsigned, so a rect alone cannot say "flipped". Both `glBlitFramebuffer` and
/// `vkCmdBlitImage` express a mirror by inverting a rect's bounds (`x1 < x0`), which a surface must
/// normalize with a min/max before it has an origin and an extent at all — that normalization is where
/// the intent used to be discarded. The surface carries it here instead: the NET flip of an axis is the
/// source inversion exclusive-or the destination inversion, since inverting both sides mirrors twice and
/// is the identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mirror {
    pub x: bool,
    pub y: bool,
}

impl Mirror {
    /// No mirroring on either axis — an ordinary blit.
    pub const NONE: Self = Mirror { x: false, y: false };

    /// The net mirror of a blit whose source and destination rects were each inverted (or not) per axis.
    pub fn net(src: Self, dst: Self) -> Self {
        Mirror {
            x: src.x != dst.x,
            y: src.y != dst.y,
        }
    }

    /// Wire form: bit 0 = `x`, bit 1 = `y`. Any other bit is not a mirror this version defines.
    pub fn to_u32(self) -> u32 {
        u32::from(self.x) | (u32::from(self.y) << 1)
    }

    /// Inverse of [`Self::to_u32`]; `None` for a value carrying a bit this version does not define, so an
    /// unknown mirroring is a decode error rather than silently an unmirrored blit.
    pub fn from_u32(v: u32) -> Option<Self> {
        (v & !0b11 == 0).then_some(Mirror {
            x: v & 0b01 != 0,
            y: v & 0b10 != 0,
        })
    }
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
