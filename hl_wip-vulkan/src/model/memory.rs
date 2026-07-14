//! Buffer / device-memory / image / sampler records + the Vulkan→hl-GPU usage & format translation.
//!
//! Ported from `hl-shim-vk/src/memory.rs` (`vkCreateBuffer`/`vkAllocateMemory`/`vkBindBufferMemory`/
//! `vkCreateImage`/`vkCreateSampler` bodies + `buffer_usage`/`texture_usage`/`tex_format`), themselves
//! mirroring MoltenVK's `MVKBuffer`/`MVKDeviceMemory`/`MVKImage`/`MVKSampler`. Pure value types + pure
//! translation functions from the stable Vulkan enum/flag ABI onto the neutral [`hl_gpu`] vocabulary;
//! the record ids are minted by [`super::device::Device`], which owns the id counters.

use crate::{VkBuffer, VkDeviceMemory};
use hl_gpu::protocol::model::enums::TextureFormat;

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
    pub bound_buffer: Option<VkBuffer>,
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
    pub format: TextureFormat,
    /// hl-GPU `texture_usage` bits (translated from `VkImageUsageFlags`).
    pub usage: u32,
    pub is_render_target: bool,
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
    pub const R8G8B8A8_UNORM: u32 = 37;
    pub const R8G8B8A8_SRGB: u32 = 43;
    pub const B8G8R8A8_UNORM: u32 = 44;
    pub const B8G8R8A8_SRGB: u32 = 50;
    pub const R8_UNORM: u32 = 9;
    pub const R8G8_UNORM: u32 = 16;
    pub const R16G16B16A16_SFLOAT: u32 = 97;
    pub const R32G32B32A32_SFLOAT: u32 = 109;
    pub const R32_SFLOAT: u32 = 100;
    pub const D32_SFLOAT: u32 = 126;
    pub const D24_UNORM_S8_UINT: u32 = 129;
}

/// Translate `VkBufferUsageFlags` → hl-GPU `buffer_usage` bits. Ported from `memory.rs::buffer_usage`.
/// Every hl device buffer additionally gets `MAP` (unified memory is host-visible).
pub fn buffer_usage_from_vk(u: u32) -> u32 {
    use hl_gpu::protocol::model::enums::buffer_usage as bu;
    let mut out = bu::MAP;
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

/// Translate `VkImageUsageFlags` → hl-GPU `texture_usage` bits. Ported from `memory.rs::texture_usage`.
pub fn texture_usage_from_vk(u: u32) -> u32 {
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
pub fn is_render_target(u: u32) -> bool {
    u & (vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::DEPTH_STENCIL_ATTACHMENT) != 0
}

/// Translate `VkFormat` → hl-GPU `TextureFormat` (the color/depth subset the bring-up render path
/// needs). Ported from `memory.rs::tex_format`; an unmapped format folds to `Rgba8Unorm`.
pub fn tex_format_from_vk(f: u32) -> TextureFormat {
    use TextureFormat as T;
    match f {
        vk_format::R8G8B8A8_UNORM => T::Rgba8Unorm,
        vk_format::R8G8B8A8_SRGB => T::Rgba8Srgb,
        vk_format::B8G8R8A8_UNORM => T::Bgra8Unorm,
        vk_format::B8G8R8A8_SRGB => T::Bgra8Srgb,
        vk_format::R8_UNORM => T::R8Unorm,
        vk_format::R8G8_UNORM => T::Rg8Unorm,
        vk_format::R16G16B16A16_SFLOAT => T::Rgba16Float,
        vk_format::R32G32B32A32_SFLOAT => T::Rgba32Float,
        vk_format::R32_SFLOAT => T::R32Float,
        vk_format::D32_SFLOAT => T::Depth32Float,
        vk_format::D24_UNORM_S8_UINT => T::Depth24PlusStencil8,
        _ => T::Rgba8Unorm,
    }
}
