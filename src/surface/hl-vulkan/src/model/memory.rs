//! Buffer / device-memory / image / sampler records + the Vulkan→hl-GPU usage & format translation.
//!
//! Ported from `hl-shim-vk/src/memory.rs` (`vkCreateBuffer`/`vkAllocateMemory`/`vkBindBufferMemory`/
//! `vkCreateImage`/`vkCreateSampler` bodies + `buffer_usage`/`texture_usage`/`tex_format`), themselves
//! mirroring MoltenVK's `MVKBuffer`/`MVKDeviceMemory`/`MVKImage`/`MVKSampler`. Pure value types + pure
//! translation functions from the stable Vulkan enum/flag ABI onto the neutral [`hl_gpu`] vocabulary;
//! the record ids are minted by [`super::device::Device`], which owns the id counters.

use crate::{VkBuffer, VkDeviceMemory};
use hl_gpu::protocol::model::enums::{TextureDim, TextureFormat};

/// One `VkBuffer`: its backing hl-GPU buffer id + size + (translated) usage, and the device memory it
/// is bound to (`vkBindBufferMemory`), if any. Mirrors MoltenVK `MVKBuffer`.
#[derive(Clone, PartialEq, Debug)]
pub struct BufferRec {
    pub ir_id: u32,
    pub size: u64,
    /// hl-GPU `buffer_usage` bits (translated from `VkBufferUsageFlags`).
    pub usage: u32,
    pub bound_mem: Option<VkDeviceMemory>,
    pub bound_offset: u64,
}

/// One `VkDeviceMemory`: the host-visible bytes (unified memory), its size, the buffer bound into it,
/// and whether it is currently mapped. A persistently-mapped HOST_COHERENT allocation is re-uploaded
/// to the host each `vkQueueSubmit` (MoltenVK/vkcube pattern). Mirrors `MVKDeviceMemory`.
///
/// `pending_flush` closes the map → write → **unmap** → submit data-loss edge: a real app that stages
/// into a mapped buffer and then `vkUnmapMemory`s BEFORE submitting would otherwise lose the upload,
/// because `mapped` is now `false` and the still-mapped flush no longer sees it. `vkUnmapMemory` (and
/// `vkFlushMappedMemoryRanges` for the non-coherent contract) instead record the dirtied `(offset,
/// size)` range here; the next `vkQueueSubmit` flushes it as a `Cmd::WriteBuffer` and clears it. The
/// still-mapped path and this pending path are coalesced (a memory yields at most one upload per
/// submit — see [`super::device::Device::mapped_uploads`]), so no byte is written twice.
#[derive(Clone, PartialEq, Debug)]
pub struct MemRec {
    pub data: Vec<u8>,
    pub size: u64,
    /// Every `VkBuffer` bound into this allocation (`vkBindBufferMemory`), each at its own
    /// `BufferRec::bound_offset`. A single allocation routinely backs MANY buffers — the sub-allocating
    /// arena pattern of gpu-alloc/VMA (e.g. blade/GPUI binds hundreds of uniform/storage/vertex buffers
    /// into one big HOST_COHERENT block). Tracking only the last-bound buffer here silently dropped the
    /// host→device flush of every OTHER buffer in the arena (their device bytes stayed zero — a fully
    /// blank frame), so this is the full set: each is flushed/refreshed against its own footprint.
    pub bound_buffers: Vec<VkBuffer>,
    pub mapped: bool,
    /// A captured host→device upload range `(offset, size)` (allocation coordinates; `size ==
    /// VK_WHOLE_SIZE` → to the end) that must still flush at the next submit even though the app may
    /// have unmapped. `None` when there is nothing pending. Only meaningful for buffer-bound memory.
    pub pending_flush: Option<(u64, u64)>,
}

/// One `VkImage`: its backing hl-GPU texture id + geometry/format/(translated) usage. Mirrors `MVKImage`.
#[derive(Clone, PartialEq, Debug)]
pub struct ImageRec {
    pub ir_id: u32,
    pub width: u32,
    pub height: u32,
    /// Base-level depth for a 3D image. Always one for 1D/2D images.
    pub depth: u32,
    /// Vulkan image geometry; array layers and depth are distinct axes.
    pub dim: TextureDim,
    pub layers: u32,
    pub mip_levels: u32,
    pub format: TextureFormat,
    /// hl-GPU `texture_usage` bits (translated from `VkImageUsageFlags`).
    pub usage: u32,
    /// Multisample count (`VkSampleCountFlagBits` → the count: 1/2/4/8/…). `1` for a single-sample image.
    /// A `vkCmdResolveImage` whose SOURCE carries `sample_count > 1` lowers to a true `Enc::ResolveTexture`
    /// (a multisample resolve); a single-sample source is a same-extent copy — see `record::cmd_resolve_image`.
    pub sample_count: u32,
    pub is_render_target: bool,
}

impl ImageRec {
    /// This image's extent AT `mip`, which is what a copy region naming that level is measured against.
    /// Validating a region against the BASE level instead let a copy naming a higher mip pass a bounds
    /// check it should have failed, because every smaller level fits inside level zero.
    pub fn extent_at(&self, mip: u32) -> (u32, u32) {
        ((self.width >> mip).max(1), (self.height >> mip).max(1))
    }

    /// This image's DEPTH at `mip` — the bound a depth-spanning blit region is measured against.
    ///
    /// Vulkan halves a 3D image's depth per level exactly as it halves width and height, floored and
    /// never below one, which is why this mirrors [`Self::extent_at`] rather than returning `depth`.
    /// `depth` is one for every non-3D image (`vkCreateImage` refuses anything else), so this returns
    /// one for them at every level, which is the correct bound for a region Vulkan pins at z 0..1.
    pub fn depth_at(&self, mip: u32) -> u32 {
        (self.depth >> mip).max(1)
    }
}

/// A `VkImageSubresourceLayers` — the single mip level and array-layer run a copy, blit or resolve
/// region addresses. Distinct from [`SubresourceRange`]: a range spans levels, a *layers* value names
/// exactly one, because a copy region's extent belongs to that one level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubresourceLayers {
    pub mip_level: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
    /// The Vulkan `aspectMask` the region named.
    ///
    /// This field did not exist, so `vkCmdCopyImage`, `vkCmdBlitImage` and `vkCmdResolveImage` discarded
    /// the aspect before it reached the IR and every one of them recorded `TextureAspect::All`. On a
    /// combined depth/stencil image that is a silent wrong answer rather than a lost detail: a guest
    /// asking to copy the DEPTH plane of a `D24_UNORM_S8_UINT` image got both planes, and the
    /// destination's stencil was overwritten with no error. The buffer-to-image sibling in the same shim
    /// has always inspected `aspectMask`; the image-to-image path never did.
    pub aspect_mask: u32,
}

impl SubresourceLayers {
    /// The base subresource: mip 0, layer 0, one layer, the colour aspect.
    pub fn base() -> Self {
        Self {
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
            aspect_mask: Self::ASPECT_COLOR,
        }
    }

    pub const ASPECT_COLOR: u32 = 0x0000_0001;
    pub const ASPECT_DEPTH: u32 = 0x0000_0002;
    pub const ASPECT_STENCIL: u32 = 0x0000_0004;

    /// Resolve `VK_REMAINING_ARRAY_LAYERS` against `image`, leaving everything else untouched so an
    /// out-of-range level or layer stays out of range and is refused rather than silently clamped — a
    /// copy is not a clear, and quietly shrinking one returns the wrong texels instead of fewer.
    pub fn resolve(image: &ImageRec, layers: Self) -> Self {
        if layers.layer_count != SubresourceRange::REMAINING {
            return layers;
        }
        Self {
            layer_count: image.layers.saturating_sub(layers.base_array_layer),
            ..layers
        }
    }
}

/// A `VkImageSubresourceRange`'s mip and array-layer extent, with Vulkan's `VK_REMAINING_*` sentinels
/// already resolved against the image they name.
///
/// The sentinels are resolved at construction rather than at each use so no caller can forget: the value
/// that reaches the recorder is always a concrete run of levels and layers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubresourceRange {
    pub base_mip_level: u32,
    pub level_count: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

impl SubresourceRange {
    /// `VK_REMAINING_MIP_LEVELS` / `VK_REMAINING_ARRAY_LAYERS`.
    pub const REMAINING: u32 = u32::MAX;

    /// Resolve a raw range against `image`, clamping each run to the levels and layers that exist. A range
    /// naming nothing that exists resolves to a zero-length run, which records no work.
    pub fn resolve(image: &ImageRec, range: Self) -> Self {
        let levels = range.remaining(range.base_mip_level, range.level_count, image.mip_levels);
        let layers = range.remaining(range.base_array_layer, range.layer_count, image.layers);
        Self {
            base_mip_level: range.base_mip_level.min(image.mip_levels),
            level_count: levels,
            base_array_layer: range.base_array_layer.min(image.layers),
            layer_count: layers,
        }
    }

    fn remaining(&self, base: u32, count: u32, available: u32) -> u32 {
        let rest = available.saturating_sub(base);
        if count == Self::REMAINING {
            rest
        } else {
            count.min(rest)
        }
    }

    /// The whole image — the range a caller means when it clears without naming a subresource.
    pub fn whole(image: &ImageRec) -> Self {
        Self {
            base_mip_level: 0,
            level_count: image.mip_levels,
            base_array_layer: 0,
            layer_count: image.layers,
        }
    }
}

/// One `VkSampler`: the backing hl-GPU sampler id. Mirrors `MVKSampler`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SamplerRec {
    pub ir_id: u32,
}

// ---- stable Vulkan flag/enum ABI subsets (re-declared clean-room from vk.xml) --------------------

/// `VkBufferUsageFlagBits` (stable bit values from vk.xml).
pub mod vk_buffer_usage {
    pub const TRANSFER_SRC: u32 = 0x0000_0001;
    pub const TRANSFER_DST: u32 = 0x0000_0002;
    pub const UNIFORM_TEXEL_BUFFER: u32 = 0x0000_0004;
    pub const STORAGE_TEXEL_BUFFER: u32 = 0x0000_0008;
    pub const UNIFORM_BUFFER: u32 = 0x0000_0010;
    pub const STORAGE_BUFFER: u32 = 0x0000_0020;
    pub const INDEX_BUFFER: u32 = 0x0000_0040;
    pub const VERTEX_BUFFER: u32 = 0x0000_0080;
    pub const INDIRECT_BUFFER: u32 = 0x0000_0100;
}

/// `VkImageUsageFlagBits` (stable bit values from vk.xml).
pub mod vk_image_usage {
    pub const TRANSFER_SRC: u32 = 0x0000_0001;
    pub const TRANSFER_DST: u32 = 0x0000_0002;
    pub const SAMPLED: u32 = 0x0000_0004;
    pub const STORAGE: u32 = 0x0000_0008;
    pub const COLOR_ATTACHMENT: u32 = 0x0000_0010;
    pub const DEPTH_STENCIL_ATTACHMENT: u32 = 0x0000_0020;
}

/// `VkFormat` (stable enum values from vk.xml) for the color/depth subset the render path needs.
pub mod vk_format {
    pub const B4G4R4A4_UNORM_PACK16: u32 = 3;
    pub const R5G6B5_UNORM_PACK16: u32 = 4;
    pub const A1R5G5B5_UNORM_PACK16: u32 = 8;
    pub const R8G8B8A8_UNORM: u32 = 37;
    pub const R8G8B8A8_SRGB: u32 = 43;
    pub const B8G8R8A8_UNORM: u32 = 44;
    pub const B8G8R8A8_SRGB: u32 = 50;
    pub const A8B8G8R8_UNORM_PACK32: u32 = 51;
    pub const A8B8G8R8_SNORM_PACK32: u32 = 52;
    pub const A8B8G8R8_UINT_PACK32: u32 = 55;
    pub const A8B8G8R8_SINT_PACK32: u32 = 56;
    pub const A8B8G8R8_SRGB_PACK32: u32 = 57;
    pub const R8_UNORM: u32 = 9;
    pub const R8_SNORM: u32 = 10;
    pub const R8G8_UNORM: u32 = 16;
    pub const R8G8_SNORM: u32 = 17;
    pub const R8G8B8A8_SNORM: u32 = 38;
    // Integer color. Unfilterable and unblendable by specification: a shader reads them only through
    // `texelFetch`/`textureLoad` and their texels are raw integers, never normalized.
    pub const R8_UINT: u32 = 13;
    pub const R8_SINT: u32 = 14;
    pub const R8G8_UINT: u32 = 20;
    pub const R8G8_SINT: u32 = 21;
    pub const R8G8B8A8_UINT: u32 = 41;
    pub const R8G8B8A8_SINT: u32 = 42;
    pub const R16G16B16A16_SFLOAT: u32 = 97;
    pub const R16G16_SFLOAT: u32 = 83;
    pub const R16_UNORM: u32 = 70;
    pub const R16_SNORM: u32 = 71;
    pub const R16_UINT: u32 = 74;
    pub const R16_SINT: u32 = 75;
    pub const R16_SFLOAT: u32 = 76;
    pub const R16G16_UNORM: u32 = 77;
    pub const R16G16_SNORM: u32 = 78;
    pub const R16G16_UINT: u32 = 81;
    pub const R16G16_SINT: u32 = 82;
    pub const R16G16B16A16_UNORM: u32 = 91;
    pub const R16G16B16A16_SNORM: u32 = 92;
    pub const R16G16B16A16_UINT: u32 = 95;
    pub const R16G16B16A16_SINT: u32 = 96;
    pub const R32G32B32A32_SFLOAT: u32 = 109;
    pub const R32_UINT: u32 = 98;
    pub const R32_SINT: u32 = 99;
    pub const R32_SFLOAT: u32 = 100;
    pub const R32G32_UINT: u32 = 101;
    pub const R32G32_SINT: u32 = 102;
    pub const R32G32_SFLOAT: u32 = 103;
    pub const R32G32B32A32_UINT: u32 = 107;
    pub const R32G32B32A32_SINT: u32 = 108;
    pub const D16_UNORM: u32 = 124;
    pub const D32_SFLOAT: u32 = 126;
    pub const D24_UNORM_S8_UINT: u32 = 129;
    pub const BC1_RGB_UNORM_BLOCK: u32 = 131;
    pub const BC1_RGB_SRGB_BLOCK: u32 = 132;
    pub const BC1_RGBA_UNORM_BLOCK: u32 = 133;
    pub const BC1_RGBA_SRGB_BLOCK: u32 = 134;
    pub const BC2_UNORM_BLOCK: u32 = 135;
    pub const BC2_SRGB_BLOCK: u32 = 136;
    pub const BC3_UNORM_BLOCK: u32 = 137;
    pub const BC3_SRGB_BLOCK: u32 = 138;
    pub const BC4_UNORM_BLOCK: u32 = 139;
    pub const BC4_SNORM_BLOCK: u32 = 140;
    pub const BC5_UNORM_BLOCK: u32 = 141;
    pub const BC5_SNORM_BLOCK: u32 = 142;
    pub const BC6H_UFLOAT_BLOCK: u32 = 143;
    pub const BC6H_SFLOAT_BLOCK: u32 = 144;
    pub const BC7_UNORM_BLOCK: u32 = 145;
    pub const BC7_SRGB_BLOCK: u32 = 146;
    pub const ETC2_R8G8B8_UNORM_BLOCK: u32 = 147;
    pub const ETC2_R8G8B8_SRGB_BLOCK: u32 = 148;
    pub const ETC2_R8G8B8A1_UNORM_BLOCK: u32 = 149;
    pub const ETC2_R8G8B8A1_SRGB_BLOCK: u32 = 150;
    pub const ETC2_R8G8B8A8_UNORM_BLOCK: u32 = 151;
    pub const ETC2_R8G8B8A8_SRGB_BLOCK: u32 = 152;
    pub const EAC_R11_UNORM_BLOCK: u32 = 153;
    pub const EAC_R11_SNORM_BLOCK: u32 = 154;
    pub const EAC_R11G11_UNORM_BLOCK: u32 = 155;
    pub const EAC_R11G11_SNORM_BLOCK: u32 = 156;
    pub const E5B9G9R9_UFLOAT_PACK32: u32 = 123;
    pub const A2B10G10R10_UNORM_PACK32: u32 = 64;
    pub const A2B10G10R10_UINT_PACK32: u32 = 68;
    pub const B10G11R11_UFLOAT_PACK32: u32 = 122;
}

/// `VkFormat` values that appear as VERTEX ATTRIBUTE formats. Distinct from the image formats above
/// because the wire encodes a vertex attribute completely differently from a texture (see
/// [`VertexFormat`]), and because Vulkan permits component counts and widths here that no image uses.
pub mod vk_vertex_format {
    // 8-bit
    pub const R8_UNORM: u32 = 9;
    pub const R8_SNORM: u32 = 10;
    pub const R8_UINT: u32 = 13;
    pub const R8_SINT: u32 = 14;
    pub const R8G8_UNORM: u32 = 16;
    pub const R8G8_SNORM: u32 = 17;
    pub const R8G8_UINT: u32 = 20;
    pub const R8G8_SINT: u32 = 21;
    pub const R8G8B8A8_UNORM: u32 = 37;
    pub const R8G8B8A8_SNORM: u32 = 38;
    pub const R8G8B8A8_UINT: u32 = 41;
    pub const R8G8B8A8_SINT: u32 = 42;
    // BGRA component order — legal Vulkan vertex formats with no wire encoding, kept named so the
    // refusal is deliberate and testable rather than an unlisted gap.
    pub const B8G8R8A8_UNORM: u32 = 44;
    pub const B8G8R8A8_SNORM: u32 = 45;
    pub const B8G8R8A8_UINT: u32 = 48;
    pub const B8G8R8A8_SINT: u32 = 49;
    pub const A8B8G8R8_UNORM_PACK32: u32 = 51;
    pub const A8B8G8R8_SNORM_PACK32: u32 = 52;
    pub const A8B8G8R8_UINT_PACK32: u32 = 55;
    pub const A8B8G8R8_SINT_PACK32: u32 = 56;
    // packed
    pub const A2B10G10R10_UNORM_PACK32: u32 = 64;
    // 16-bit
    pub const R16_UNORM: u32 = 70;
    pub const R16_SNORM: u32 = 71;
    pub const R16_UINT: u32 = 74;
    pub const R16_SINT: u32 = 75;
    pub const R16_SFLOAT: u32 = 76;
    pub const R16G16_UNORM: u32 = 77;
    pub const R16G16_SNORM: u32 = 78;
    pub const R16G16_UINT: u32 = 81;
    pub const R16G16_SINT: u32 = 82;
    pub const R16G16_SFLOAT: u32 = 83;
    pub const R16G16B16A16_UNORM: u32 = 91;
    pub const R16G16B16A16_SNORM: u32 = 92;
    pub const R16G16B16A16_UINT: u32 = 95;
    pub const R16G16B16A16_SINT: u32 = 96;
    pub const R16G16B16A16_SFLOAT: u32 = 97;
    // 32-bit
    pub const R32_UINT: u32 = 98;
    pub const R32_SINT: u32 = 99;
    pub const R32_SFLOAT: u32 = 100;
    pub const R32G32_UINT: u32 = 101;
    pub const R32G32_SINT: u32 = 102;
    pub const R32G32_SFLOAT: u32 = 103;
    pub const R32G32B32_UINT: u32 = 104;
    pub const R32G32B32_SINT: u32 = 105;
    pub const R32G32B32_SFLOAT: u32 = 106;
    pub const R32G32B32A32_UINT: u32 = 107;
    pub const R32G32B32A32_SINT: u32 = 108;
    pub const R32G32B32A32_SFLOAT: u32 = 109;
}

/// Translate a `VkVertexInputAttributeDescription::format` into the neutral wire encoding the executor
/// decodes for a vertex attribute.
///
/// The wire field is NOT a format enum of any API. It is a packed quadruple —
/// `comps | (kind << 8) | (normalized << 16) | (integer << 17)`, `kind` 0=f32 1=u8 2=i8 3=u16 4=i16
/// 5=u32 6=i32 7=f16 8=2-10-10-10 — defined by the GL driver's `vertex_format_wire` and decoded by
/// `hl-gpu-wgpu`'s `VertexState::format`. Passing a raw `VkFormat` through the field therefore does not
/// mean the format it names: `VK_FORMAT_R32G32B32_SFLOAT` is 106, which decodes as a 106-component
/// attribute and is refused by the executor as an unsupported vertex attribute format. That is what made
/// every `vkmark` scene with geometry fail at `vkCreateGraphicsPipelines` with `VK_ERROR_DEVICE_LOST`
/// while `vkcube` — which sources its positions from a uniform buffer by `gl_VertexIndex` and declares no
/// vertex bindings at all — presented perfectly.
///
/// `None` means the executor has no encoding for this attribute and the pipeline must be REFUSED rather
/// than created against a format the host will silently reinterpret. WebGPU has no 1- or 3-component
/// 8-/16-bit vertex format, and the wire cannot express a BGRA component order, so those Vulkan formats
/// are honestly unsupported instead of being widened or swizzled behind the application's back.
pub struct VertexFormat(pub u32);

impl VertexFormat {
    /// Wire `kind` codes, mirroring the GL driver's encoder.
    const F32: u32 = 0;
    const U8: u32 = 1;
    const I8: u32 = 2;
    const U16: u32 = 3;
    const I16: u32 = 4;
    const U32: u32 = 5;
    const I32: u32 = 6;
    const F16: u32 = 7;
    const PACKED_2_10_10_10: u32 = 8;
    const BGRA_U8: u32 = 9;

    pub fn wire(&self) -> Option<u32> {
        use vk_vertex_format as f;
        // (kind, components, normalized, integer)
        let (kind, comps, normalized, integer) = match self.0 {
            // 32-bit float — the overwhelmingly common case (positions, normals, uvs, colors).
            f::R32_SFLOAT => (Self::F32, 1, false, false),
            f::R32G32_SFLOAT => (Self::F32, 2, false, false),
            f::R32G32B32_SFLOAT => (Self::F32, 3, false, false),
            f::R32G32B32A32_SFLOAT => (Self::F32, 4, false, false),
            // 32-bit integer
            f::R32_UINT => (Self::U32, 1, false, true),
            f::R32G32_UINT => (Self::U32, 2, false, true),
            f::R32G32B32_UINT => (Self::U32, 3, false, true),
            f::R32G32B32A32_UINT => (Self::U32, 4, false, true),
            f::R32_SINT => (Self::I32, 1, false, true),
            f::R32G32_SINT => (Self::I32, 2, false, true),
            f::R32G32B32_SINT => (Self::I32, 3, false, true),
            f::R32G32B32A32_SINT => (Self::I32, 4, false, true),
            // 16-bit float — x2/x4 only.
            f::R16_SFLOAT => (Self::F16, 1, false, false),
            f::R16G16_SFLOAT => (Self::F16, 2, false, false),
            f::R16G16B16A16_SFLOAT => (Self::F16, 4, false, false),
            // 8-bit — x2/x4 only.
            f::R8_UNORM => (Self::U8, 1, true, false),
            f::R8_SNORM => (Self::I8, 1, true, false),
            f::R8_UINT => (Self::U8, 1, false, true),
            f::R8_SINT => (Self::I8, 1, false, true),
            f::R8G8_UNORM => (Self::U8, 2, true, false),
            f::R8G8B8A8_UNORM => (Self::U8, 4, true, false),
            f::R8G8_UINT => (Self::U8, 2, false, true),
            f::R8G8B8A8_UINT => (Self::U8, 4, false, true),
            f::R8G8_SNORM => (Self::I8, 2, true, false),
            f::R8G8B8A8_SNORM => (Self::I8, 4, true, false),
            f::B8G8R8A8_UNORM => (Self::BGRA_U8, 4, true, false),
            f::R8G8_SINT => (Self::I8, 2, false, true),
            f::R8G8B8A8_SINT => (Self::I8, 4, false, true),
            // Vulkan's A8B8G8R8_PACK32 has little-endian bytes R,G,B,A, exactly the executor's x4 layout.
            f::A8B8G8R8_UNORM_PACK32 => (Self::U8, 4, true, false),
            f::A8B8G8R8_SNORM_PACK32 => (Self::I8, 4, true, false),
            f::A8B8G8R8_UINT_PACK32 => (Self::U8, 4, false, true),
            f::A8B8G8R8_SINT_PACK32 => (Self::I8, 4, false, true),
            // 16-bit integer — x2/x4 only.
            f::R16_UINT => (Self::U16, 1, false, true),
            f::R16_SINT => (Self::I16, 1, false, true),
            f::R16_UNORM => (Self::U16, 1, true, false),
            f::R16_SNORM => (Self::I16, 1, true, false),
            f::R16G16_UNORM => (Self::U16, 2, true, false),
            f::R16G16B16A16_UNORM => (Self::U16, 4, true, false),
            f::R16G16_UINT => (Self::U16, 2, false, true),
            f::R16G16B16A16_UINT => (Self::U16, 4, false, true),
            f::R16G16_SNORM => (Self::I16, 2, true, false),
            f::R16G16B16A16_SNORM => (Self::I16, 4, true, false),
            f::R16G16_SINT => (Self::I16, 2, false, true),
            f::R16G16B16A16_SINT => (Self::I16, 4, false, true),
            // The one packed format the wire encodes, and only in its normalized 4-component form.
            f::A2B10G10R10_UNORM_PACK32 => (Self::PACKED_2_10_10_10, 4, true, false),
            // Everything else — 1- and 3-component 8-/16-bit formats, every BGRA order, every sRGB,
            // scaled and 64-bit format — has no wire encoding. Refused, never approximated.
            _ => return None,
        };
        Some(comps | (kind << 8) | ((normalized as u32) << 16) | ((integer as u32) << 17))
    }
}

/// Translate `VkBufferUsageFlags` → hl-GPU `buffer_usage` bits. Ported from `memory.rs::buffer_usage`.
/// Every hl device buffer additionally gets `MAP` (unified memory is host-visible).
pub struct BufferUsage(pub u32);

impl BufferUsage {
    pub fn wire(&self) -> u32 {
        let u = self.0;
        use hl_gpu::protocol::model::enums::buffer_usage as bu;
        let mut out = bu::MAP;
        if u & (vk_buffer_usage::UNIFORM_TEXEL_BUFFER | vk_buffer_usage::STORAGE_TEXEL_BUFFER) != 0
        {
            // Texel views bind the original allocation as packed raw storage; copy usage remains available
            // to the executor's protocol-level transfer paths.
            out |= bu::STORAGE | bu::COPY_SRC | bu::COPY_DST;
        }
        if u & vk_buffer_usage::STORAGE_BUFFER != 0 {
            out |= bu::STORAGE;
        }
        if u & vk_buffer_usage::UNIFORM_BUFFER != 0 {
            out |= bu::UNIFORM;
        }
        if u & vk_buffer_usage::VERTEX_BUFFER != 0 {
            out |= bu::VERTEX;
        }
        if u & vk_buffer_usage::INDEX_BUFFER != 0 {
            out |= bu::INDEX;
        }
        if u & vk_buffer_usage::TRANSFER_SRC != 0 {
            out |= bu::COPY_SRC;
        }
        if u & vk_buffer_usage::TRANSFER_DST != 0 {
            out |= bu::COPY_DST;
        }
        if u & vk_buffer_usage::INDIRECT_BUFFER != 0 {
            out |= bu::INDIRECT;
        }
        out
    }
}

/// Translate `VkImageUsageFlags` → hl-GPU `texture_usage` bits. Ported from `memory.rs::texture_usage`.
pub struct ImageUsage(pub u32);

impl ImageUsage {
    pub fn wire(&self) -> u32 {
        let u = self.0;
        use hl_gpu::protocol::model::enums::texture_usage as tu;
        let mut out = 0;
        if u & vk_image_usage::SAMPLED != 0 {
            out |= tu::SAMPLED;
        }
        if u & vk_image_usage::STORAGE != 0 {
            out |= tu::STORAGE;
        }
        if u & (vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::DEPTH_STENCIL_ATTACHMENT) != 0 {
            out |= tu::RENDER_TARGET;
        }
        if u & vk_image_usage::TRANSFER_SRC != 0 {
            out |= tu::COPY_SRC;
        }
        if u & vk_image_usage::TRANSFER_DST != 0 {
            out |= tu::COPY_DST;
        }
        out
    }

    /// Whether a `VkImageUsageFlags` marks the image as a render target (color/depth attachment).
    pub fn is_render_target(&self) -> bool {
        self.0 & (vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::DEPTH_STENCIL_ATTACHMENT) != 0
    }
}

/// Translate a supported `VkFormat` into the neutral hl-GPU format.
///
/// Unknown formats stay unknown. Substituting RGBA8 would change texel layout while reporting success,
/// causing callers to allocate and interpret a different image than the application requested.
pub struct Format(pub u32);

impl Format {
    pub fn wire(&self) -> Option<TextureFormat> {
        let f = self.0;
        use TextureFormat as T;
        Some(match f {
            vk_format::R8G8B8A8_UNORM => T::Rgba8Unorm,
            vk_format::R8G8B8A8_SRGB => T::Rgba8Srgb,
            vk_format::B8G8R8A8_UNORM => T::Bgra8Unorm,
            vk_format::B8G8R8A8_SRGB => T::Bgra8Srgb,
            vk_format::A8B8G8R8_UNORM_PACK32 => T::Rgba8Unorm,
            vk_format::A8B8G8R8_SNORM_PACK32 => T::Rgba8Snorm,
            vk_format::A8B8G8R8_UINT_PACK32 => T::Rgba8Uint,
            vk_format::A8B8G8R8_SINT_PACK32 => T::Rgba8Sint,
            vk_format::A8B8G8R8_SRGB_PACK32 => T::Rgba8Srgb,
            vk_format::R8_UNORM => T::R8Unorm,
            vk_format::R8_SNORM => T::R8Snorm,
            vk_format::R8G8_UNORM => T::Rg8Unorm,
            vk_format::R8G8_SNORM => T::Rg8Snorm,
            vk_format::R8G8B8A8_SNORM => T::Rgba8Snorm,
            vk_format::R16G16_SFLOAT => T::Rg16Float,
            vk_format::R16G16_UNORM => T::Rg16Unorm,
            vk_format::R16G16_SNORM => T::Rg16Snorm,
            vk_format::R16G16B16A16_SNORM => T::Rgba16Snorm,
            vk_format::R16_UNORM => T::R16Unorm,
            vk_format::R16_SNORM => T::R16Snorm,
            vk_format::R16_UINT => T::R16Uint,
            vk_format::R16_SINT => T::R16Sint,
            vk_format::R16_SFLOAT => T::R16Float,
            vk_format::R16G16_UINT => T::Rg16Uint,
            vk_format::R16G16_SINT => T::Rg16Sint,
            vk_format::R16G16B16A16_UINT => T::Rgba16Uint,
            vk_format::R16G16B16A16_SINT => T::Rgba16Sint,
            vk_format::R16G16B16A16_SFLOAT => T::Rgba16Float,
            vk_format::R16G16B16A16_UNORM => T::Rgba16Unorm,
            vk_format::R32G32B32A32_SFLOAT => T::Rgba32Float,
            vk_format::R32_SFLOAT => T::R32Float,
            vk_format::R32_UINT => T::R32Uint,
            vk_format::R32_SINT => T::R32Sint,
            vk_format::R32G32_UINT => T::Rg32Uint,
            vk_format::R32G32_SINT => T::Rg32Sint,
            vk_format::R32G32_SFLOAT => T::Rg32Float,
            vk_format::R32G32B32A32_UINT => T::Rgba32Uint,
            vk_format::R32G32B32A32_SINT => T::Rgba32Sint,
            // The integer color family. The neutral wire has carried these since the GL driver needed
            // `GL_RGBA_INTEGER`/`GL_RED_INTEGER`/`GL_RG_INTEGER` storage, and `hl-gpu-wgpu` maps each to its
            // exact wgpu counterpart, but this Vulkan lowering never learned them — so every `vkCreateImage`
            // naming one was refused for a format the whole stack below already materializes.
            vk_format::R8G8B8A8_UINT => T::Rgba8Uint,
            vk_format::R8G8B8A8_SINT => T::Rgba8Sint,
            vk_format::R8_UINT => T::R8Uint,
            vk_format::R8_SINT => T::R8Sint,
            vk_format::R8G8_UINT => T::Rg8Uint,
            vk_format::R8G8_SINT => T::Rg8Sint,
            // The hl model carries no 16-bit depth target; fold D16 onto the 32-bit float depth format so a
            // classic pass declaring VK_FORMAT_D16_UNORM (vkcube) resolves to a real depth aspect — both the
            // depth image and the pipeline's DepthState land on the same format, staying executor-valid.
            vk_format::D16_UNORM => T::Depth32Float,
            vk_format::D32_SFLOAT => T::Depth32Float,
            vk_format::D24_UNORM_S8_UINT => T::Depth24PlusStencil8,
            vk_format::BC1_RGB_UNORM_BLOCK | vk_format::BC1_RGBA_UNORM_BLOCK => T::Bc1RgbaUnorm,
            vk_format::BC1_RGB_SRGB_BLOCK | vk_format::BC1_RGBA_SRGB_BLOCK => T::Bc1RgbaSrgb,
            vk_format::BC2_UNORM_BLOCK => T::Bc2RgbaUnorm,
            vk_format::BC2_SRGB_BLOCK => T::Bc2RgbaSrgb,
            vk_format::BC3_UNORM_BLOCK => T::Bc3RgbaUnorm,
            vk_format::BC3_SRGB_BLOCK => T::Bc3RgbaSrgb,
            vk_format::BC4_UNORM_BLOCK => T::Bc4RUnorm,
            vk_format::BC4_SNORM_BLOCK => T::Bc4RSnorm,
            vk_format::BC5_UNORM_BLOCK => T::Bc5RgUnorm,
            vk_format::BC5_SNORM_BLOCK => T::Bc5RgSnorm,
            vk_format::BC6H_UFLOAT_BLOCK => T::Bc6hRgbUfloat,
            vk_format::BC6H_SFLOAT_BLOCK => T::Bc6hRgbFloat,
            vk_format::BC7_UNORM_BLOCK => T::Bc7RgbaUnorm,
            vk_format::BC7_SRGB_BLOCK => T::Bc7RgbaSrgb,
            vk_format::ETC2_R8G8B8_UNORM_BLOCK => T::Etc2Rgb8Unorm,
            vk_format::ETC2_R8G8B8_SRGB_BLOCK => T::Etc2Rgb8Srgb,
            vk_format::ETC2_R8G8B8A1_UNORM_BLOCK => T::Etc2Rgb8A1Unorm,
            vk_format::ETC2_R8G8B8A1_SRGB_BLOCK => T::Etc2Rgb8A1Srgb,
            vk_format::ETC2_R8G8B8A8_UNORM_BLOCK => T::Etc2Rgba8Unorm,
            vk_format::ETC2_R8G8B8A8_SRGB_BLOCK => T::Etc2Rgba8Srgb,
            vk_format::EAC_R11_UNORM_BLOCK => T::EacR11Unorm,
            vk_format::EAC_R11_SNORM_BLOCK => T::EacR11Snorm,
            vk_format::EAC_R11G11_UNORM_BLOCK => T::EacRg11Unorm,
            vk_format::EAC_R11G11_SNORM_BLOCK => T::EacRg11Snorm,
            vk_format::E5B9G9R9_UFLOAT_PACK32 => T::Rgb9e5Ufloat,
            vk_format::A2B10G10R10_UNORM_PACK32 => T::Rgb10a2Unorm,
            vk_format::A2B10G10R10_UINT_PACK32 => T::Rgb10a2Uint,
            vk_format::B10G11R11_UFLOAT_PACK32 => T::Rg11b10Ufloat,
            vk_format::R5G6B5_UNORM_PACK16 => T::R5g6b5Unorm,
            vk_format::A1R5G5B5_UNORM_PACK16 => T::A1r5g5b5Unorm,
            vk_format::B4G4R4A4_UNORM_PACK16 => T::B4g4r4a4Unorm,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod vertex_format_tests {
    use super::{vk_vertex_format as f, VertexFormat};

    /// Decode a wire value the way `hl-gpu-wgpu`'s `VertexState::format` does, so these tests assert the
    /// ENCODING CONTRACT rather than re-stating this module's own arithmetic. Every existing hl-vulkan
    /// test built its `VertexAttr` by hand with `format: 0`, which is why none of them could notice that
    /// the shim never lowered `VkFormat` at all.
    fn decode(wire: u32) -> (u32, u32, bool, bool) {
        (
            wire & 0xff,
            (wire >> 8) & 0xff,
            (wire >> 16) & 1 != 0,
            (wire >> 17) & 1 != 0,
        )
    }

    /// The formats a real application actually uses for positions, normals and texture coordinates.
    /// `R32G32B32_SFLOAT` is the one every `vkmark` geometry scene declares; forwarded raw it is 106,
    /// which the executor decoded as a 106-component attribute and refused.
    #[test]
    fn float_positions_lower_to_a_component_count_and_not_to_a_vulkan_enum() {
        for (format, comps) in [
            (f::R32_SFLOAT, 1),
            (f::R32G32_SFLOAT, 2),
            (f::R32G32B32_SFLOAT, 3),
            (f::R32G32B32A32_SFLOAT, 4),
        ] {
            let wire = VertexFormat(format)
                .wire()
                .expect("a core float format lowers");
            assert_eq!(
                decode(wire),
                (comps, VertexFormat::F32, false, false),
                "VkFormat {format} must lower to {comps} f32 components"
            );
            assert_ne!(
                wire, format,
                "the wire value must not be the VkFormat number itself"
            );
        }
    }

    /// Normalized and integer attributes must carry their flags, or the executor picks the wrong
    /// `wgpu::VertexFormat` from the same (kind, comps) pair — `Unorm8x4` versus `Uint8x4`.
    #[test]
    fn normalized_and_integer_flags_survive_the_encoding() {
        assert_eq!(
            decode(VertexFormat(f::R8G8B8A8_UNORM).wire().unwrap()),
            (4, VertexFormat::U8, true, false)
        );
        assert_eq!(
            decode(VertexFormat(f::R8G8B8A8_UINT).wire().unwrap()),
            (4, VertexFormat::U8, false, true)
        );
        assert_eq!(
            decode(VertexFormat(f::R16G16_SNORM).wire().unwrap()),
            (2, VertexFormat::I16, true, false)
        );
        assert_eq!(
            decode(VertexFormat(f::R8_SNORM).wire().unwrap()),
            (1, VertexFormat::I8, true, false)
        );
        assert_eq!(
            decode(VertexFormat(f::R16_UNORM).wire().unwrap()),
            (1, VertexFormat::U16, true, false)
        );
        assert_eq!(
            decode(VertexFormat(f::R16_SNORM).wire().unwrap()),
            (1, VertexFormat::I16, true, false)
        );
        assert_eq!(
            decode(VertexFormat(f::A8B8G8R8_UNORM_PACK32).wire().unwrap()),
            (4, VertexFormat::U8, true, false)
        );
        assert_eq!(
            decode(VertexFormat(f::R16G16B16A16_SFLOAT).wire().unwrap()),
            (4, VertexFormat::F16, false, false)
        );
        assert_eq!(
            decode(VertexFormat(f::A2B10G10R10_UNORM_PACK32).wire().unwrap()),
            (4, VertexFormat::PACKED_2_10_10_10, true, false)
        );
    }

    /// A format the wire cannot express is refused, never widened or swizzled. WebGPU has no
    /// 3-component 8-/16-bit vertex format, and the wire has no BGRA component order, so honouring these
    /// would mean handing the executor an attribute that reads different bytes than the app declared.
    #[test]
    fn a_format_without_a_wire_encoding_is_refused_rather_than_approximated() {
        for format in [
            f::B8G8R8A8_SINT,
            0,
            u32::MAX,
        ] {
            assert!(
                VertexFormat(format).wire().is_none(),
                "VkFormat {format} has no wire encoding and must be refused"
            );
        }
    }

    /// Every lowered format must decode to a component count Vulkan agrees with, and to one the executor
    /// accepts. Catches a transcription slip in the table that a per-format assertion would miss.
    #[test]
    fn every_lowered_format_decodes_to_a_legal_attribute() {
        for format in 0..=200u32 {
            let Some(wire) = VertexFormat(format).wire() else {
                continue;
            };
            let (comps, kind, normalized, integer) = decode(wire);
            assert!(
                (1..=4).contains(&comps),
                "VkFormat {format} lowered to {comps} components"
            );
            assert!(
                !(normalized && integer),
                "VkFormat {format} is both normalized and integer"
            );
            assert!(kind <= VertexFormat::BGRA_U8, "VkFormat {format} lowered to unknown kind {kind}");
        }
    }
}
