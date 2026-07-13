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

#[no_mangle]
pub extern "C" fn vkAllocateMemory(
    _device: VkDevice,
    p_alloc_info: *const vk::MemoryAllocateInfo,
    _p_allocator: *const c_void,
    p_memory: *mut VkDeviceMemory,
) -> VkResult {
    let (Some(ai), Some(out)) = (unsafe { p_alloc_info.as_ref() }, unsafe { p_memory.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.memories.insert(
        handle,
        MemRec {
            data: vec![0u8; ai.allocation_size as usize],
            bound_buffer: None,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkFreeMemory(_device: VkDevice, memory: VkDeviceMemory, _p_allocator: *const c_void) {
    reg::lock().memories.remove(&memory);
}

#[no_mangle]
pub extern "C" fn vkBindBufferMemory(
    _device: VkDevice,
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    memory_offset: u64,
) -> VkResult {
    let mut s = reg::lock();
    if let Some(b) = s.buffers.get_mut(&buffer) {
        b.bound_mem = Some(memory);
        b.bound_offset = memory_offset;
    }
    if let Some(m) = s.memories.get_mut(&memory) {
        m.bound_buffer = Some(buffer);
    }
    VK_SUCCESS
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
    _size: u64,
    _flags: u32,
    pp_data: *mut *mut c_void,
) -> VkResult {
    let Some(out) = (unsafe { pp_data.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let Some(m) = s.memories.get_mut(&memory) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    *out = unsafe { m.data.as_mut_ptr().add(offset as usize) } as *mut c_void;
    VK_SUCCESS
}

/// `vkUnmapMemory` — flush the mapped staging to the bound buffer as an IR `WriteBuffer` (the H2D
/// upload). Ported from `MVKDeviceMemory::unmap`/`flushToDevice`.
#[no_mangle]
pub extern "C" fn vkUnmapMemory(_device: VkDevice, memory: VkDeviceMemory) {
    let mut s = reg::lock();
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

#[no_mangle]
pub extern "C" fn vkBindImageMemory(
    _device: VkDevice,
    _image: VkImage,
    _memory: VkDeviceMemory,
    _offset: u64,
) -> VkResult {
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkCreateImageView(
    _device: VkDevice,
    p_create_info: *const vk::ImageViewCreateInfo,
    _p_allocator: *const c_void,
    p_view: *mut VkImageView,
) -> VkResult {
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
