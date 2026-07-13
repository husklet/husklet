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

use crate::reg::{self, BufferRec, ImageRec, ImageViewRec, MemRec};
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
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    let is_rt = ci.usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT);
    s.images.insert(
        handle,
        ImageRec {
            ir_id,
            width: ci.extent.width,
            height: ci.extent.height,
            format: tex_format(ci.format),
            is_render_target: is_rt,
            bound_mem: None,
        },
    );
    // A color attachment is host-owned (registered by the host as a render target); non-attachment
    // sampled/storage images would emit Cmd::CreateTexture in a later increment.
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyImage(_device: VkDevice, image: VkImage, _p_allocator: *const c_void) {
    reg::lock().images.remove(&image);
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
    let handle = s.alloc_handle();
    s.image_views.insert(
        handle,
        ImageViewRec {
            image: ci.image.as_raw(),
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyImageView(_device: VkDevice, view: VkImageView, _p_allocator: *const c_void) {
    reg::lock().image_views.remove(&view);
}
