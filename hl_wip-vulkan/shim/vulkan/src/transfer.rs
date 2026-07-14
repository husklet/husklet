//! The IR-wired TRANSFER path: the hand-written `vkCmd*` bodies for buffer/image copies, blits, clears,
//! buffer fills/updates, and pipeline barriers — marshalling the Vulkan C ABI and calling the
//! `hl_vulkan` [`record`](hl_vulkan::service::record) lowering services.
//!
//! These are `void` commands: a structurally invalid call (null pointer, unknown handle, out-of-range
//! region, missing usage) is a benign no-op (it records nothing), exactly as a real driver's validation
//! layer would reject it. Each region of a multi-region copy lowers to one encoder op; buffer
//! fills/updates flush as `Cmd::WriteBuffer` at the owning `vkQueueSubmit`. Pipeline barriers are honest
//! correctness bookkeeping (the hl-GPU IR is layout-implicit) — they record the image layout transition
//! and emit no encoder op. Ported from `hl-shim-vk/src/command.rs`.

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::with;
use crate::types::*;

/// Run `f` with just the logical device (recording bodies emit into the command buffer, not the sink).
fn dev<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
    with(|s| s.device.as_mut().map(f))
}

/// Unwrap a dispatchable `VkCommandBuffer` to its `hl_vulkan` `u64` command-buffer handle.
unsafe fn cmdbuf_handle(p: *mut c_void) -> Option<VkCbHandle> {
    Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
}

// ---- buffer / image copies -----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdCopyBuffer(
    command_buffer: *mut c_void,
    src_buffer: u64,
    dst_buffer: u64,
    region_count: u32,
    p_regions: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if p_regions.is_null() {
        return;
    }
    let regions = unsafe { std::slice::from_raw_parts(p_regions as *const VkBufferCopy, region_count as usize) };
    dev(|d| {
        for r in regions {
            let _ = record::cmd_copy_buffer(d, cb, src_buffer, dst_buffer, r.src_offset, r.dst_offset, r.size);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBufferToImage(
    command_buffer: *mut c_void,
    src_buffer: u64,
    dst_image: u64,
    _dst_image_layout: i32,
    region_count: u32,
    p_regions: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if p_regions.is_null() {
        return;
    }
    let regions =
        unsafe { std::slice::from_raw_parts(p_regions as *const VkBufferImageCopy, region_count as usize) };
    dev(|d| {
        for r in regions {
            // The base color subresource only (mip 0 / layer 0 / origin 0) — the materialized subset.
            if r.image_subresource.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0
                || r.image_subresource.mip_level != 0
                || r.image_subresource.base_array_layer != 0
                || r.image_offset.x != 0
                || r.image_offset.y != 0
            {
                continue;
            }
            let _ = record::cmd_copy_buffer_to_image(
                d,
                cb,
                src_buffer,
                dst_image,
                r.buffer_offset,
                r.buffer_row_length,
                r.buffer_image_height,
                r.image_extent.width,
                r.image_extent.height.max(1),
            );
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImageToBuffer(
    command_buffer: *mut c_void,
    src_image: u64,
    _src_image_layout: i32,
    dst_buffer: u64,
    region_count: u32,
    p_regions: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if p_regions.is_null() {
        return;
    }
    let regions =
        unsafe { std::slice::from_raw_parts(p_regions as *const VkBufferImageCopy, region_count as usize) };
    dev(|d| {
        for r in regions {
            if r.image_subresource.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0
                || r.image_subresource.mip_level != 0
                || r.image_subresource.base_array_layer != 0
                || r.image_offset.x != 0
                || r.image_offset.y != 0
            {
                continue;
            }
            let _ = record::cmd_copy_image_to_buffer(
                d,
                cb,
                src_image,
                dst_buffer,
                r.buffer_offset,
                r.buffer_row_length,
                r.buffer_image_height,
                r.image_extent.width,
                r.image_extent.height.max(1),
            );
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImage(
    command_buffer: *mut c_void,
    src_image: u64,
    _src_image_layout: i32,
    dst_image: u64,
    _dst_image_layout: i32,
    region_count: u32,
    p_regions: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if p_regions.is_null() {
        return;
    }
    let regions = unsafe { std::slice::from_raw_parts(p_regions as *const VkImageCopy, region_count as usize) };
    dev(|d| {
        for r in regions {
            if r.src_offset.x < 0 || r.src_offset.y < 0 || r.dst_offset.x < 0 || r.dst_offset.y < 0 {
                continue;
            }
            let _ = record::cmd_copy_image(
                d,
                cb,
                src_image,
                dst_image,
                (r.src_offset.x as u32, r.src_offset.y as u32),
                (r.dst_offset.x as u32, r.dst_offset.y as u32),
                (r.extent.width, r.extent.height.max(1)),
            );
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdBlitImage(
    command_buffer: *mut c_void,
    src_image: u64,
    _src_image_layout: i32,
    dst_image: u64,
    _dst_image_layout: i32,
    region_count: u32,
    p_regions: *const c_void,
    filter: i32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if p_regions.is_null() {
        return;
    }
    let regions = unsafe { std::slice::from_raw_parts(p_regions as *const VkImageBlit, region_count as usize) };
    let linear = filter == VK_FILTER_LINEAR;
    dev(|d| {
        for r in regions {
            let [s0, s1] = r.src_offsets;
            let [d0, d1] = r.dst_offsets;
            // Only forward, in-plane (z: 0->1) blits map onto the unsigned IR origin/extent.
            if s1.x <= s0.x || s1.y <= s0.y || s0.z != 0 || s1.z != 1
                || d1.x <= d0.x || d1.y <= d0.y || d0.z != 0 || d1.z != 1
            {
                continue;
            }
            let _ = record::cmd_blit_image(
                d,
                cb,
                src_image,
                dst_image,
                (s0.x as u32, s0.y as u32),
                ((s1.x - s0.x) as u32, (s1.y - s0.y) as u32),
                (d0.x as u32, d0.y as u32),
                ((d1.x - d0.x) as u32, (d1.y - d0.y) as u32),
                linear,
            );
        }
    });
}

// ---- clears --------------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdClearColorImage(
    command_buffer: *mut c_void,
    image: u64,
    _image_layout: i32,
    p_color: *const c_void,
    _range_count: u32,
    _p_ranges: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    let Some(color) = (unsafe { (p_color as *const VkClearColorValue).as_ref() }) else { return };
    let rgba = color.float32;
    dev(|d| {
        let _ = record::cmd_clear_color_image(d, cb, image, rgba);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdClearAttachments(
    command_buffer: *mut c_void,
    attachment_count: u32,
    p_attachments: *const c_void,
    rect_count: u32,
    p_rects: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if attachment_count == 0 || rect_count == 0 || p_attachments.is_null() || p_rects.is_null() {
        return;
    }
    let attachments =
        unsafe { std::slice::from_raw_parts(p_attachments as *const VkClearAttachment, attachment_count as usize) };
    let rects = unsafe { std::slice::from_raw_parts(p_rects as *const VkClearRect, rect_count as usize) };
    dev(|d| {
        for att in attachments {
            if att.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0 {
                continue; // depth/stencil clears are not materialized in this bring-up model
            }
            let color = att.clear_value.float32;
            for r in rects {
                let _ = record::cmd_clear_attachment_rect(
                    d,
                    cb,
                    r.rect.offset.x.max(0) as u32,
                    r.rect.offset.y.max(0) as u32,
                    r.rect.extent.width,
                    r.rect.extent.height,
                    color,
                );
            }
        }
    });
}

// ---- buffer fills / updates ----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdFillBuffer(
    command_buffer: *mut c_void,
    dst_buffer: u64,
    dst_offset: u64,
    size: u64,
    data: u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    dev(|d| {
        let _ = record::cmd_fill_buffer(d, cb, dst_buffer, dst_offset, size, data);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdUpdateBuffer(
    command_buffer: *mut c_void,
    dst_buffer: u64,
    dst_offset: u64,
    data_size: u64,
    p_data: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    if p_data.is_null() || data_size == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p_data as *const u8, data_size as usize) };
    dev(|d| {
        let _ = record::cmd_update_buffer(d, cb, dst_buffer, dst_offset, bytes);
    });
}

// ---- pipeline barriers ---------------------------------------------------------------------------

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdPipelineBarrier(
    command_buffer: *mut c_void,
    _src_stage_mask: u32,
    _dst_stage_mask: u32,
    _dependency_flags: u32,
    _memory_barrier_count: u32,
    _p_memory_barriers: *const c_void,
    _buffer_memory_barrier_count: u32,
    _p_buffer_memory_barriers: *const c_void,
    image_memory_barrier_count: u32,
    p_image_memory_barriers: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    let barriers = if image_memory_barrier_count == 0 || p_image_memory_barriers.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                p_image_memory_barriers as *const VkImageMemoryBarrier,
                image_memory_barrier_count as usize,
            )
        }
    };
    let transitions: Vec<(u64, i32, i32)> =
        barriers.iter().map(|b| (b.image, b.old_layout, b.new_layout)).collect();
    dev(|d| {
        let _ = record::cmd_pipeline_barrier(d, cb, &transitions);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdPipelineBarrier2(command_buffer: *mut c_void, p_dependency_info: *const c_void) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    let Some(dep) = (unsafe { (p_dependency_info as *const VkDependencyInfo).as_ref() }) else { return };
    let barriers = if dep.image_memory_barrier_count == 0 || dep.p_image_memory_barriers.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(dep.p_image_memory_barriers, dep.image_memory_barrier_count as usize)
        }
    };
    let transitions: Vec<(u64, i32, i32)> =
        barriers.iter().map(|b| (b.image, b.old_layout, b.new_layout)).collect();
    dev(|d| {
        let _ = record::cmd_pipeline_barrier(d, cb, &transitions);
    });
}
