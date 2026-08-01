//! Clear, fill, and update commands.

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::SubresourceRange;

use super::{CommandBuffer, ShimState};
use crate::types::*;

pub extern "C" fn vkCmdClearColorImage(
    command_buffer: *mut c_void,
    image: u64,
    _image_layout: i32,
    p_color: *const c_void,
    range_count: u32,
    p_ranges: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(color) = (unsafe { (p_color as *const VkClearColorValue).as_ref() }) else {
        return;
    };
    // pRanges says WHICH mip levels and array layers to clear. Dropping it made every clear land on the
    // base subresource, so a layered or mipped image was cleared in the wrong place.
    let ranges: Vec<SubresourceRange> = if range_count == 0 || p_ranges.is_null() {
        Vec::new()
    } else {
        unsafe {
            std::slice::from_raw_parts(
                p_ranges as *const VkImageSubresourceRange,
                range_count as usize,
            )
        }
        .iter()
        .map(|range| SubresourceRange {
            base_mip_level: range.base_mip_level,
            level_count: range.level_count,
            base_array_layer: range.base_array_layer,
            layer_count: range.layer_count,
        })
        .collect()
    };
    ShimState::with_device(|device| {
        let _ =
            record::cmd_clear_color_image(device, command_buffer, image, color.float32, &ranges);
    });
}

pub extern "C" fn vkCmdClearAttachments(
    command_buffer: *mut c_void,
    attachment_count: u32,
    p_attachments: *const c_void,
    rect_count: u32,
    p_rects: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if attachment_count == 0 || rect_count == 0 || p_attachments.is_null() || p_rects.is_null() {
        return;
    }
    let attachments = unsafe {
        std::slice::from_raw_parts(
            p_attachments as *const VkClearAttachment,
            attachment_count as usize,
        )
    };
    let rects =
        unsafe { std::slice::from_raw_parts(p_rects as *const VkClearRect, rect_count as usize) };
    ShimState::with_device(|device| {
        for attachment in attachments {
            // Mid-pass depth/stencil rectangle clears cannot be represented by the current IR.
            if attachment.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0 {
                continue;
            }
            for rect in rects {
                let recorded = record::cmd_clear_attachment_rect(
                    device,
                    command_buffer,
                    rect.rect.offset.x.max(0) as u32,
                    rect.rect.offset.y.max(0) as u32,
                    rect.rect.extent.width,
                    rect.rect.extent.height,
                    attachment.clear_value.float32,
                );
                device.latch(command_buffer, recorded);
            }
        }
    });
}

pub extern "C" fn vkCmdFillBuffer(
    command_buffer: *mut c_void,
    dst_buffer: u64,
    dst_offset: u64,
    size: u64,
    data: u32,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|device| {
        let recorded =
            record::cmd_fill_buffer(device, command_buffer, dst_buffer, dst_offset, size, data);
        device.latch(command_buffer, recorded);
    });
}

pub extern "C" fn vkCmdUpdateBuffer(
    command_buffer: *mut c_void,
    dst_buffer: u64,
    dst_offset: u64,
    data_size: u64,
    p_data: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_data.is_null() || data_size == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p_data as *const u8, data_size as usize) };
    ShimState::with_device(|device| {
        let recorded =
            record::cmd_update_buffer(device, command_buffer, dst_buffer, dst_offset, bytes);
        device.latch(command_buffer, recorded);
    });
}
