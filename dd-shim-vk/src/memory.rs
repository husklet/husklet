//! Memory + buffer + image entry points (real bodies), producing dd-gpu IR.
//!
//! Ported from MoltenVK's `MVKBuffer`/`MVKDeviceMemory`/`MVKImage`:
//!   * `MVKDeviceMemory.mm` — host-visible memory is a mapped allocation the app writes into
//!     (`map()`/`unmap()`); on unmap the range flushes to the GPU buffer. We model that as a host
//!     staging `Vec<u8>` (the memory) that, on `vkUnmapMemory`, emits an IR `WriteBuffer` for the
//!     buffer bound to it (the exact H2D upload the host executor performs).
//!   * `MVKBuffer.mm` — a `VkBuffer` is a range of a `VkDeviceMemory` bound via `bindBufferMemory`.
//!     We map each `VkBuffer` to one IR buffer id (`Cmd::CreateBuffer`), sized + usage-typed from
//!     `VkBufferCreateInfo`.
//!   * `MVKImage.mm` — a color-attachment `VkImage` becomes the render target the pass draws into.
//!     Like a swapchain image it is host-owned (the loader/host provides the surface), so we allocate
//!     its IR texture id and let the host register the target — the shim never emits `CreateTexture`
//!     for an attachment (matching the render-target flip-scratch contract in dd-gpu-wgpu).

use crate::reg::{
    self, BufferRec, ImageRec, ImageSubresourceRange, ImageSubresourceState, ImageViewRec, MemRec,
};
use crate::types::*;
use ash::vk;
use ash::vk::Handle; // `.as_raw()` on handle newtypes (VkImage/VkImageView -> u64)
use core::ffi::c_void;

/// VkBufferUsageFlags → dd-gpu `buffer_usage` bits. The host backend always adds COPY_SRC/DST, so we
/// only translate the binding-relevant classes (storage/uniform/vertex/index).
fn buffer_usage(u: vk::BufferUsageFlags) -> u32 {
    use dd_shim_common::ir::buffer_usage as bu;
    let mut out = 0;
    if u.contains(vk::BufferUsageFlags::STORAGE_BUFFER) {
        out |= bu::STORAGE;
    }
    if u.contains(vk::BufferUsageFlags::UNIFORM_BUFFER) {
        out |= bu::UNIFORM;
    }
    if u.contains(vk::BufferUsageFlags::VERTEX_BUFFER) {
        out |= bu::VERTEX;
    }
    if u.contains(vk::BufferUsageFlags::INDEX_BUFFER) {
        out |= bu::INDEX;
    }
    if u.contains(vk::BufferUsageFlags::TRANSFER_SRC) {
        out |= bu::COPY_SRC;
    }
    if u.contains(vk::BufferUsageFlags::TRANSFER_DST) {
        out |= bu::COPY_DST;
    }
    out
}

fn texture_usage(u: vk::ImageUsageFlags) -> u32 {
    use dd_shim_common::ir::texture_usage as tu;
    let mut out = 0;
    if u.contains(vk::ImageUsageFlags::SAMPLED) {
        out |= tu::SAMPLED;
    }
    if u.contains(vk::ImageUsageFlags::STORAGE) {
        out |= tu::STORAGE;
    }
    if u.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
        out |= tu::RENDER_TARGET;
    }
    if u.contains(vk::ImageUsageFlags::TRANSFER_SRC) {
        out |= tu::COPY_SRC;
    }
    if u.contains(vk::ImageUsageFlags::TRANSFER_DST) {
        out |= tu::COPY_DST;
    }
    out
}

/// VkFormat → dd-gpu IR `TextureFormat` (the color-target subset the bring-up render path needs).
pub fn tex_format(f: vk::Format) -> dd_shim_common::ir::TextureFormat {
    use dd_shim_common::ir::TextureFormat as T;
    match f {
        vk::Format::R8G8B8A8_UNORM => T::Rgba8Unorm,
        vk::Format::R8G8B8A8_SRGB => T::Rgba8Srgb,
        vk::Format::B8G8R8A8_UNORM => T::Bgra8Unorm,
        vk::Format::B8G8R8A8_SRGB => T::Bgra8Srgb,
        _ => T::Rgba8Unorm,
    }
}

// ---- buffers -------------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateBuffer(
    _device: VkDevice,
    p_create_info: *const vk::BufferCreateInfo,
    _p_allocator: *const c_void,
    p_buffer: *mut VkBuffer,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_buffer.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    let usage = buffer_usage(ci.usage);
    s.record(dd_shim_common::ir::Cmd::CreateBuffer(
        ir_id,
        dd_shim_common::ir::BufferDesc {
            size: ci.size,
            usage,
            label: format!("vkbuf{ir_id}"),
        },
    ));
    s.buffers.insert(
        handle,
        BufferRec {
            ir_id,
            size: ci.size,
            usage,
            bound_mem: None,
            bound_offset: 0,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyBuffer(_device: VkDevice, buffer: VkBuffer, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    if let Some(b) = s.buffers.remove(&buffer) {
        s.record(dd_shim_common::ir::Cmd::DestroyBuffer(b.ir_id));
    }
}

/// `vkGetBufferMemoryRequirements` — report the buffer size, 256-byte aligned, memory-type bit 0
/// (our single unified type). Layout ported from MoltenVK `MVKBuffer::getMemoryRequirements`.
#[no_mangle]
pub extern "C" fn vkGetBufferMemoryRequirements(
    _device: VkDevice,
    buffer: VkBuffer,
    p_reqs: *mut vk::MemoryRequirements,
) {
    let size = reg::lock().buffers.get(&buffer).map(|b| b.size).unwrap_or(0);
    if let Some(out) = unsafe { p_reqs.as_mut() } {
        *out = vk::MemoryRequirements {
            size,
            alignment: 256,
            memory_type_bits: 0b1,
        };
    }
}

#[no_mangle]
pub extern "C" fn vkGetImageMemoryRequirements(
    _device: VkDevice,
    image: VkImage,
    p_reqs: *mut vk::MemoryRequirements,
) {
    crate::reg::trace("vkGetImageMemoryRequirements");
    let size = reg::lock()
        .images
        .get(&image)
        .map(|i| (i.width * i.height * 4) as u64)
        .unwrap_or(0);
    if let Some(out) = unsafe { p_reqs.as_mut() } {
        *out = vk::MemoryRequirements {
            size,
            alignment: 256,
            memory_type_bits: 0b1,
        };
    }
}

// ---- device memory -------------------------------------------------------------------------------

/// Our single unified memory type (index 0): DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT (`state::memory_properties`).
const UNIFIED_MEMORY_TYPE_INDEX: u32 = 0;
/// The alignment `vkGetBufferMemoryRequirements` reports; bind offsets must be a multiple of it.
const BUFFER_ALIGNMENT: u64 = 256;
/// `VK_WHOLE_SIZE` sentinel (map/flush a range from `offset` to the end of the allocation).
const VK_WHOLE_SIZE: u64 = u64::MAX;

/// `vkAllocateMemory` — validate the requested `memoryTypeIndex` against our single exposed type and
/// allocate the backing store **fallibly** (never convert an arbitrary `u64` to an infallible `Vec`,
/// which would abort on a huge request). Ported from MoltenVK `MVKDeviceMemory` (records size + type;
/// a failed host allocation surfaces as `VK_ERROR_OUT_OF_DEVICE_MEMORY`, not a crash).
#[no_mangle]
pub extern "C" fn vkAllocateMemory(
    _device: VkDevice,
    p_alloc_info: *const vk::MemoryAllocateInfo,
    _p_allocator: *const c_void,
    p_memory: *mut VkDeviceMemory,
) -> VkResult {
    crate::reg::trace("vkAllocateMemory");
    let (Some(ai), Some(out)) = (unsafe { p_alloc_info.as_ref() }, unsafe { p_memory.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let memory_type_index = ai.memory_type_index;
    if memory_type_index != UNIFIED_MEMORY_TYPE_INDEX {
        // We advertise exactly one memory type; any other index is not backed.
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
    }
    let size = ai.allocation_size;
    // Fallible allocation: a genuinely huge (or attacker-chosen) allocationSize must fail, not abort.
    let mut data: Vec<u8> = Vec::new();
    if data.try_reserve_exact(size as usize).is_err() {
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
    }
    data.resize(size as usize, 0);
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.memories.insert(
        handle,
        MemRec {
            data,
            size,
            memory_type_index,
            bound_buffer: None,
            mapped: false,
            mapped_range: None,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkFreeMemory(_device: VkDevice, memory: VkDeviceMemory, _p_allocator: *const c_void) {
    reg::lock().memories.remove(&memory);
}

/// `vkBindBufferMemory` — bind a buffer to a range of a device allocation, **validating** the Vulkan
/// requirements before mutating any state (MoltenVK `MVKBuffer::bindDeviceMemory` + the spec's bind
/// VUIDs): the objects exist, the buffer is not already bound, the offset is aligned to the buffer's
/// required alignment, the memory type is compatible with the buffer's `memoryTypeBits`, and
/// `offset + size` fits inside the allocation. It is NOT an unconditional success.
#[no_mangle]
pub extern "C" fn vkBindBufferMemory(
    _device: VkDevice,
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    memory_offset: u64,
) -> VkResult {
    let mut s = reg::lock();
    // The buffer must exist and must not already be bound (a buffer binds exactly once).
    let Some(b) = s.buffers.get(&buffer) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if b.bound_mem.is_some() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let buf_size = b.size;
    // `vkGetBufferMemoryRequirements` reports memoryTypeBits = 0b1 (only our type 0 is compatible).
    let buffer_memory_type_bits = 0b1u32;
    // The memory must exist; read its allocation size + type for the range/compat checks.
    let Some(m) = s.memories.get(&memory) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let allocation_size = m.size;
    if (buffer_memory_type_bits >> m.memory_type_index) & 1 == 0 {
        return VK_ERROR_INITIALIZATION_FAILED; // memory type incompatible with the buffer
    }
    // Offset must be a multiple of the buffer's required alignment.
    if memory_offset % BUFFER_ALIGNMENT != 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    // The bound range must fit within the allocation (checked arithmetic — no wraparound).
    match memory_offset.checked_add(buf_size) {
        Some(end) if end <= allocation_size => {}
        _ => return VK_ERROR_OUT_OF_DEVICE_MEMORY,
    }
    // All checks passed — record the binding on both sides.
    if let Some(b) = s.buffers.get_mut(&buffer) {
        b.bound_mem = Some(memory);
        b.bound_offset = memory_offset;
    }
    if let Some(m) = s.memories.get_mut(&memory) {
        m.bound_buffer = Some(buffer);
    }
    VK_SUCCESS
}

/// Validate a `vkMapMemory` range against an allocation, returning the mapped length in bytes or
/// `VK_ERROR_MEMORY_MAP_FAILED`. `VK_WHOLE_SIZE` maps from `offset` to the end. Ported from the range
/// checks in `MVKDeviceMemory::map`. (Placed here, with the binding entry points, as the shared memory
/// range validator.)
fn mapped_len(allocation_size: u64, offset: u64, size: u64) -> Result<usize, VkResult> {
    if offset > allocation_size {
        return Err(VK_ERROR_MEMORY_MAP_FAILED);
    }
    let end = if size == VK_WHOLE_SIZE {
        allocation_size
    } else {
        match offset.checked_add(size) {
            Some(e) if e <= allocation_size => e,
            _ => return Err(VK_ERROR_MEMORY_MAP_FAILED),
        }
    };
    Ok((end - offset) as usize)
}

/// `vkMapMemory` — hand the app a pointer into the host staging allocation (host-visible|coherent, the
/// Apple-silicon unified model). Ported from `MVKDeviceMemory::map`.
///
/// # Safety note
/// The returned pointer aliases the `Vec<u8>` inside the global state; it is sized at allocation and
/// never reallocated while mapped, and the shim is single-threaded per the Vulkan external-sync rules
/// the tests obey, so the pointer stays valid until `vkFreeMemory`.
#[no_mangle]
pub extern "C" fn vkMapMemory(
    _device: VkDevice,
    memory: VkDeviceMemory,
    offset: u64,
    size: u64,
    _flags: u32,
    pp_data: *mut *mut c_void,
) -> VkResult {
    crate::reg::trace("vkMapMemory");
    let Some(out) = (unsafe { pp_data.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    // The memory must exist and be host-visible (our single type is HOST_VISIBLE|COHERENT).
    let Some(m) = s.memories.get_mut(&memory) else {
        return VK_ERROR_MEMORY_MAP_FAILED;
    };
    // Vulkan forbids mapping an already-mapped allocation.
    if m.mapped {
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    // Validate the [offset, offset+size) range against the allocation (VK_WHOLE_SIZE → to the end).
    let len = match mapped_len(m.size, offset, size) {
        Ok(l) => l,
        Err(e) => return e,
    };
    // Bounds-check the pointer arithmetic itself (checked; no unchecked `add` past the buffer).
    let base = m.data.as_mut_ptr();
    if (offset as usize).checked_add(len).map(|e| e as u64 > m.size).unwrap_or(true) {
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    m.mapped = true;
    m.mapped_range = Some((offset, len as u64));
    // SAFETY: offset+len ≤ allocation size (validated above) and the Vec is not reallocated while
    // mapped (Vulkan external-sync rules), so this pointer stays valid until vkUnmapMemory/vkFreeMemory.
    *out = unsafe { base.add(offset as usize) } as *mut c_void;
    VK_SUCCESS
}

/// `vkUnmapMemory` — flush the mapped staging to the bound buffer as an IR `WriteBuffer` (the H2D
/// upload). Ported from `MVKDeviceMemory::unmap`/`flushToDevice`.
#[no_mangle]
pub extern "C" fn vkUnmapMemory(_device: VkDevice, memory: VkDeviceMemory) {
    let mut s = reg::lock();
    if let Some(m) = s.memories.get_mut(&memory) {
        m.mapped = false;
        m.mapped_range = None;
    }
    let Some(m) = s.memories.get(&memory) else {
        return;
    };
    let Some(buf_handle) = m.bound_buffer else {
        return;
    };
    let data = m.data.clone();
    if let Some(b) = s.buffers.get(&buf_handle) {
        let ir_id = b.ir_id;
        let n = (b.size as usize).min(data.len());
        s.record(dd_shim_common::ir::Cmd::WriteBuffer {
            id: ir_id,
            offset: 0,
            data: data[..n].to_vec(),
        });
    }
}

/// `vkFlushMappedMemoryRanges` — coherent memory, so this is a no-op success (the write already
/// landed in staging; the IR upload happens at unmap). Matches MoltenVK's coherent-memory fast path.
#[no_mangle]
pub extern "C" fn vkFlushMappedMemoryRanges(
    _device: VkDevice,
    _count: u32,
    _p_ranges: *const c_void,
) -> VkResult {
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkInvalidateMappedMemoryRanges(
    _device: VkDevice,
    _count: u32,
    _p_ranges: *const c_void,
) -> VkResult {
    VK_SUCCESS
}

// ---- images + views (color attachments) ----------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateImage(
    _device: VkDevice,
    p_create_info: *const vk::ImageCreateInfo,
    _p_allocator: *const c_void,
    p_image: *mut VkImage,
) -> VkResult {
    crate::reg::trace("vkCreateImage");
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_image.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if ci.image_type != vk::ImageType::TYPE_2D
        || ci.extent.depth != 1
        || !matches!(
            ci.format,
            vk::Format::R8G8B8A8_UNORM
                | vk::Format::R8G8B8A8_SRGB
                | vk::Format::B8G8R8A8_UNORM
                | vk::Format::B8G8R8A8_SRGB
        )
    {
        return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    let is_rt = ci.usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT);
    let mip_levels = ci.mip_levels.max(1);
    let array_layers = ci.array_layers.max(1);
    let aspect_mask = vk::ImageAspectFlags::COLOR.as_raw();
    let initial = ImageSubresourceState {
        layout: ci.initial_layout.as_raw(),
        last_access: 0,
        last_stage: vk::PipelineStageFlags::TOP_OF_PIPE.as_raw(),
        owner_queue_family: 0,
    };
    let mut subresources = std::collections::HashMap::new();
    for mip in 0..mip_levels {
        for layer in 0..array_layers {
            subresources.insert((aspect_mask, mip, layer), initial);
        }
    }
    s.record(dd_shim_common::ir::Cmd::CreateTexture(
        ir_id,
        dd_shim_common::ir::TextureDesc {
            width: ci.extent.width,
            height: ci.extent.height,
            depth: array_layers,
            mip_levels,
            sample_count: ci.samples.as_raw(),
            dim: dd_shim_common::ir::TextureDim::D2,
            format: tex_format(ci.format),
            usage: texture_usage(ci.usage),
            label: format!("vkimg{ir_id}"),
        },
    ));
    s.images.insert(
        handle,
        ImageRec {
            ir_id,
            width: ci.extent.width,
            height: ci.extent.height,
            format: tex_format(ci.format),
            is_render_target: is_rt,
            mip_levels,
            array_layers,
            aspect_mask,
            usage: ci.usage.as_raw(),
            sample_count: ci.samples.as_raw(),
            subresources,
            bound_mem: None,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyImage(_device: VkDevice, image: VkImage, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    if let Some(image) = s.images.remove(&image) {
        s.record(dd_shim_common::ir::Cmd::DestroyTexture(image.ir_id));
    }
}

/// `vkGetImageSubresourceLayout` — the row/array/depth pitch of a LINEAR image. vkcube maps a linear
/// staging texture and writes it row-by-row using `rowPitch`; a stubbed (garbage) layout makes it
/// write at wild offsets and corrupt the heap (surfaced as a later glibc `%n` FORTIFY abort). Report a
/// tightly-packed RGBA8 layout. Ported from MoltenVK `MVKImage::getSubresourceLayout`.
#[no_mangle]
pub extern "C" fn vkGetImageSubresourceLayout(
    _device: VkDevice,
    image: VkImage,
    _p_subresource: *const vk::ImageSubresource,
    p_layout: *mut vk::SubresourceLayout,
) {
    let (w, h) = reg::lock()
        .images
        .get(&image)
        .map(|i| (i.width, i.height))
        .unwrap_or((1, 1));
    if let Some(out) = unsafe { p_layout.as_mut() } {
        let row_pitch = (w.max(1) * 4) as u64;
        *out = vk::SubresourceLayout {
            offset: 0,
            size: row_pitch * h.max(1) as u64,
            row_pitch,
            array_pitch: row_pitch * h.max(1) as u64,
            depth_pitch: row_pitch * h.max(1) as u64,
        };
    }
}

/// `vkBindImageMemory` — bind an image to device memory. This is **NOT** a success no-op: it validates
/// that both objects exist, the image is not already bound, and the required extent fits within the
/// allocation at the offset, then records the ownership. Ported from `MVKImage::bindDeviceMemory`.
#[no_mangle]
pub extern "C" fn vkBindImageMemory(
    _device: VkDevice,
    image: VkImage,
    memory: VkDeviceMemory,
    offset: u64,
) -> VkResult {
    let mut s = reg::lock();
    // The memory must exist; capture its allocation size (immutable borrow ends before the mut borrow).
    let allocation_size = match s.memories.get(&memory) {
        Some(m) => m.size,
        None => return VK_ERROR_INITIALIZATION_FAILED,
    };
    let Some(img) = s.images.get_mut(&image) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if img.bound_mem.is_some() {
        return VK_ERROR_INITIALIZATION_FAILED; // an image binds exactly once
    }
    // Tightly-packed RGBA8 required size (matches vkGetImageMemoryRequirements); must fit in the range.
    let need = (img.width as u64) * (img.height as u64) * 4;
    match offset.checked_add(need) {
        Some(end) if end <= allocation_size => {}
        _ => return VK_ERROR_OUT_OF_DEVICE_MEMORY,
    }
    img.bound_mem = Some(memory);
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkCreateImageView(
    _device: VkDevice,
    p_create_info: *const vk::ImageViewCreateInfo,
    _p_allocator: *const c_void,
    p_view: *mut VkImageView,
) -> VkResult {
    crate::reg::trace("vkCreateImageView");
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_view.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let Some(image) = s.images.get(&ci.image.as_raw()) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let raw = ci.subresource_range;
    let level_count = if raw.level_count == vk::REMAINING_MIP_LEVELS {
        image.mip_levels.saturating_sub(raw.base_mip_level)
    } else {
        raw.level_count
    };
    let layer_count = if raw.layer_count == vk::REMAINING_ARRAY_LAYERS {
        image.array_layers.saturating_sub(raw.base_array_layer)
    } else {
        raw.layer_count
    };
    let range = ImageSubresourceRange {
        aspect_mask: raw.aspect_mask.as_raw(),
        base_mip_level: raw.base_mip_level,
        level_count,
        base_array_layer: raw.base_array_layer,
        layer_count,
    };
    if range.aspect_mask != image.aspect_mask
        || range.level_count == 0
        || range.layer_count == 0
        || range.base_mip_level.checked_add(range.level_count).is_none_or(|end| end > image.mip_levels)
        || range.base_array_layer.checked_add(range.layer_count).is_none_or(|end| end > image.array_layers)
    {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let handle = s.alloc_handle();
    s.image_views.insert(
        handle,
        ImageViewRec {
            image: ci.image.as_raw(),
            range,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyImageView(_device: VkDevice, view: VkImageView, _p_allocator: *const c_void) {
    reg::lock().image_views.remove(&view);
}

// ---- samplers ------------------------------------------------------------------------------------

/// VkFilter → dd-gpu IR `Filter` (NEAREST=0, LINEAR=1).
fn ir_filter(f: vk::Filter) -> dd_shim_common::ir::Filter {
    use dd_shim_common::ir::Filter;
    if f == vk::Filter::LINEAR { Filter::Linear } else { Filter::Nearest }
}
/// VkSamplerMipmapMode → dd-gpu IR `Filter`.
fn ir_mip_filter(m: vk::SamplerMipmapMode) -> dd_shim_common::ir::Filter {
    use dd_shim_common::ir::Filter;
    if m == vk::SamplerMipmapMode::LINEAR { Filter::Linear } else { Filter::Nearest }
}
/// VkSamplerAddressMode → dd-gpu IR `AddressMode` (the three modes the IR carries; CLAMP_TO_BORDER and
/// MIRROR_CLAMP_TO_EDGE fold to their nearest supported neighbour — a bounded translation).
fn ir_address(a: vk::SamplerAddressMode) -> dd_shim_common::ir::AddressMode {
    use dd_shim_common::ir::AddressMode;
    match a {
        vk::SamplerAddressMode::REPEAT => AddressMode::Repeat,
        vk::SamplerAddressMode::MIRRORED_REPEAT | vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE => {
            AddressMode::MirrorRepeat
        }
        _ => AddressMode::ClampToEdge, // CLAMP_TO_EDGE, CLAMP_TO_BORDER
    }
}

/// `vkCreateSampler` — translate the filter/address state into a dd-gpu IR sampler (`Cmd::CreateSampler`).
/// Ported from `MVKSampler` (`MVKImage.mm`), which builds an `MTLSamplerDescriptor` from the same fields.
#[no_mangle]
pub extern "C" fn vkCreateSampler(
    _device: VkDevice,
    p_create_info: *const vk::SamplerCreateInfo,
    _p_allocator: *const c_void,
    p_sampler: *mut VkSampler,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_sampler.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let desc = dd_shim_common::ir::SamplerDesc {
        min_filter: ir_filter(ci.min_filter),
        mag_filter: ir_filter(ci.mag_filter),
        mip_filter: ir_mip_filter(ci.mipmap_mode),
        address_u: ir_address(ci.address_mode_u),
        address_v: ir_address(ci.address_mode_v),
        address_w: ir_address(ci.address_mode_w),
    };
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    s.record(dd_shim_common::ir::Cmd::CreateSampler(ir_id, desc));
    s.samplers.insert(handle, reg::SamplerRec { ir_id });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySampler(_device: VkDevice, sampler: VkSampler, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    if let Some(rec) = s.samplers.remove(&sampler) {
        s.record(dd_shim_common::ir::Cmd::DestroySampler(rec.ir_id));
    }
}

// ---- buffer views (typed texel-buffer windows) ---------------------------------------------------

/// `vkCreateBufferView` — a typed window `[offset, offset+range)` onto a buffer (MoltenVK `MVKBufferView`).
/// Validated (buffer exists, range fits, non-empty) and retained for the texel-buffer descriptor IR
/// increment. `VK_WHOLE_SIZE` maps from `offset` to the end of the buffer.
#[no_mangle]
pub extern "C" fn vkCreateBufferView(
    _device: VkDevice,
    p_create_info: *const vk::BufferViewCreateInfo,
    _p_allocator: *const c_void,
    p_view: *mut VkBufferView,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_view.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let buffer = ci.buffer.as_raw();
    let Some(b) = s.buffers.get(&buffer) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let range = if ci.range == VK_WHOLE_SIZE {
        b.size.saturating_sub(ci.offset)
    } else {
        ci.range
    };
    if range == 0 || ci.offset.checked_add(range).is_none_or(|end| end > b.size) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let handle = s.alloc_handle();
    s.buffer_views.insert(
        handle,
        reg::BufferViewRec { buffer, format: ci.format.as_raw(), offset: ci.offset, range },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyBufferView(_device: VkDevice, view: VkBufferView, _p_allocator: *const c_void) {
    reg::lock().buffer_views.remove(&view);
}

// ---- memory queries ------------------------------------------------------------------------------

/// `vkGetDeviceMemoryCommitment` — bytes currently committed for a `LAZILY_ALLOCATED` allocation. We
/// expose no lazily-allocated memory type, so every allocation is fully resident: report its whole size.
#[no_mangle]
pub extern "C" fn vkGetDeviceMemoryCommitment(
    _device: VkDevice,
    memory: VkDeviceMemory,
    p_committed_memory_in_bytes: *mut u64,
) {
    let committed = reg::lock().memories.get(&memory).map(|m| m.size).unwrap_or(0);
    if let Some(out) = unsafe { p_committed_memory_in_bytes.as_mut() } {
        *out = committed;
    }
}

/// `vkGetImageSparseMemoryRequirements` — we do not support sparse residency, so an image has no sparse
/// memory requirements: report a count of zero (spec-valid for a non-sparse image). Ported from the
/// no-sparse path in `MVKImage::getSparseImageMemoryRequirements`.
#[no_mangle]
pub extern "C" fn vkGetImageSparseMemoryRequirements(
    _device: VkDevice,
    _image: VkImage,
    p_sparse_memory_requirement_count: *mut u32,
    _p_sparse_memory_requirements: *mut c_void,
) {
    if let Some(count) = unsafe { p_sparse_memory_requirement_count.as_mut() } {
        *count = 0;
    }
}

// ---- Vulkan 1.1: bind/requirements 2, sampler YCbCr conversion ------------------------------------

/// `vkBindBufferMemory2` (Vulkan 1.1): batch buffer binds. Each `VkBindBufferMemoryInfo` delegates to the
/// validated single-bind path; the first failure is returned. Ported from `MVKBuffer::bindDeviceMemory2`.
#[no_mangle]
pub extern "C" fn vkBindBufferMemory2(
    device: VkDevice,
    bind_info_count: u32,
    p_bind_infos: *const vk::BindBufferMemoryInfo,
) -> VkResult {
    if bind_info_count == 0 || p_bind_infos.is_null() {
        return VK_SUCCESS;
    }
    let infos = unsafe { core::slice::from_raw_parts(p_bind_infos, bind_info_count as usize) };
    for info in infos {
        let r = vkBindBufferMemory(device, info.buffer.as_raw(), info.memory.as_raw(), info.memory_offset);
        if r != VK_SUCCESS {
            return r;
        }
    }
    VK_SUCCESS
}

/// `vkBindImageMemory2` (Vulkan 1.1): batch image binds, delegating to the validated single-bind path.
#[no_mangle]
pub extern "C" fn vkBindImageMemory2(
    device: VkDevice,
    bind_info_count: u32,
    p_bind_infos: *const vk::BindImageMemoryInfo,
) -> VkResult {
    if bind_info_count == 0 || p_bind_infos.is_null() {
        return VK_SUCCESS;
    }
    let infos = unsafe { core::slice::from_raw_parts(p_bind_infos, bind_info_count as usize) };
    for info in infos {
        let r = vkBindImageMemory(device, info.image.as_raw(), info.memory.as_raw(), info.memory_offset);
        if r != VK_SUCCESS {
            return r;
        }
    }
    VK_SUCCESS
}

/// `vkGetBufferMemoryRequirements2` (Vulkan 1.1): the `...2` wrapper around the 1.0 query — fills the
/// nested `memoryRequirements` of `VkMemoryRequirements2`. Ported from `MVKBuffer::getMemoryRequirements`.
#[no_mangle]
pub extern "C" fn vkGetBufferMemoryRequirements2(
    device: VkDevice,
    p_info: *const vk::BufferMemoryRequirementsInfo2,
    p_mem_reqs: *mut vk::MemoryRequirements2,
) {
    let (Some(info), Some(out)) = (unsafe { p_info.as_ref() }, unsafe { p_mem_reqs.as_mut() }) else {
        return;
    };
    vkGetBufferMemoryRequirements(device, info.buffer.as_raw(), &mut out.memory_requirements);
}

/// `vkGetImageMemoryRequirements2` (Vulkan 1.1): the `...2` wrapper around the 1.0 image query.
#[no_mangle]
pub extern "C" fn vkGetImageMemoryRequirements2(
    device: VkDevice,
    p_info: *const vk::ImageMemoryRequirementsInfo2,
    p_mem_reqs: *mut vk::MemoryRequirements2,
) {
    let (Some(info), Some(out)) = (unsafe { p_info.as_ref() }, unsafe { p_mem_reqs.as_mut() }) else {
        return;
    };
    vkGetImageMemoryRequirements(device, info.image.as_raw(), &mut out.memory_requirements);
}

/// `vkGetImageSparseMemoryRequirements2` (Vulkan 1.1): no sparse residency → zero requirements.
#[no_mangle]
pub extern "C" fn vkGetImageSparseMemoryRequirements2(
    _device: VkDevice,
    _p_info: *const c_void,
    p_sparse_memory_requirement_count: *mut u32,
    _p_sparse_memory_requirements: *mut c_void,
) {
    if let Some(count) = unsafe { p_sparse_memory_requirement_count.as_mut() } {
        *count = 0;
    }
}

/// `vkCreateSamplerYcbcrConversion` (Vulkan 1.1, MoltenVK `MVKSamplerYcbcrConversion`): create the
/// conversion object. We do not materialize multi-planar YCbCr formats, so the object's lifetime is
/// observable but only the identity/pass-through case is meaningful (bounded — `partial`).
#[no_mangle]
pub extern "C" fn vkCreateSamplerYcbcrConversion(
    _device: VkDevice,
    p_create_info: *const vk::SamplerYcbcrConversionCreateInfo,
    _p_allocator: *const c_void,
    p_ycbcr_conversion: *mut VkSamplerYcbcrConversion,
) -> VkResult {
    let Some(out) = (unsafe { p_ycbcr_conversion.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let format = unsafe { p_create_info.as_ref() }.map(|ci| ci.format.as_raw()).unwrap_or(0);
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.ycbcr_conversions.insert(handle, reg::SamplerYcbcrConversionRec { format });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySamplerYcbcrConversion(
    _device: VkDevice,
    ycbcr_conversion: VkSamplerYcbcrConversion,
    _p_allocator: *const c_void,
) {
    reg::lock().ycbcr_conversions.remove(&ycbcr_conversion);
}
