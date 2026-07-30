//! Command-buffer operations that lower core Vulkan commands into GPU IR.

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::VkCommandBuffer as VkCbHandle;

use super::ShimState;
use crate::types::{
    Dispatchable, VkClearDepthStencilValue, VkCopyImageInfo2, VkImageCopy, VkImageSubresourceRange,
};

const VK_IMAGE_ASPECT_STENCIL_BIT: u32 = 0x0000_0004;

struct CommandBuffer;

impl CommandBuffer {
    unsafe fn handle(pointer: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(pointer).map(|handle| *handle)
    }
}

pub extern "C" fn vkCmdClearDepthStencilImage(
    command_buffer: *mut c_void,
    image: u64,
    _image_layout: i32,
    p_depth_stencil: *const c_void,
    range_count: u32,
    p_ranges: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(depth_stencil) =
        (unsafe { (p_depth_stencil as *const VkClearDepthStencilValue).as_ref() })
    else {
        return;
    };
    if p_ranges.is_null() || range_count == 0 {
        return;
    }
    let ranges = unsafe {
        std::slice::from_raw_parts(
            p_ranges as *const VkImageSubresourceRange,
            range_count as usize,
        )
    };
    let clears_stencil = ranges
        .iter()
        .any(|range| range.aspect_mask & VK_IMAGE_ASPECT_STENCIL_BIT != 0);
    ShimState::with_device(|device| {
        let _ = record::cmd_clear_depth_stencil_image(
            device,
            command_buffer,
            image,
            depth_stencil.depth,
            depth_stencil.stencil,
            clears_stencil,
        );
    });
}

pub extern "C" fn vkCmdResolveImage(
    command_buffer: *mut c_void,
    src_image: u64,
    _src_image_layout: i32,
    dst_image: u64,
    _dst_image_layout: i32,
    region_count: u32,
    p_regions: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_regions.is_null() {
        return;
    }
    let regions = unsafe {
        std::slice::from_raw_parts(p_regions as *const VkImageCopy, region_count as usize)
    };
    ShimState::with_device(|device| {
        for region in regions {
            if region.src_offset.x < 0
                || region.src_offset.y < 0
                || region.dst_offset.x < 0
                || region.dst_offset.y < 0
            {
                continue;
            }
            let _ = record::cmd_resolve_image(
                device,
                command_buffer,
                src_image,
                dst_image,
                (region.src_offset.x as u32, region.src_offset.y as u32),
                (region.dst_offset.x as u32, region.dst_offset.y as u32),
                (region.extent.width, region.extent.height.max(1)),
            );
        }
    });
}

pub extern "C" fn vkCmdResolveImage2(
    command_buffer: *mut c_void,
    p_resolve_image_info: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(info) = (unsafe { (p_resolve_image_info as *const VkCopyImageInfo2).as_ref() }) else {
        return;
    };
    if info.p_regions.is_null() {
        return;
    }
    let regions = unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
    ShimState::with_device(|device| {
        for region in regions {
            if region.src_offset.x < 0
                || region.src_offset.y < 0
                || region.dst_offset.x < 0
                || region.dst_offset.y < 0
            {
                continue;
            }
            let _ = record::cmd_resolve_image(
                device,
                command_buffer,
                info.src_image,
                info.dst_image,
                (region.src_offset.x as u32, region.src_offset.y as u32),
                (region.dst_offset.x as u32, region.dst_offset.y as u32),
                (region.extent.width, region.extent.height.max(1)),
            );
        }
    });
}

pub extern "C" fn vkCmdResolveImage2KHR(
    command_buffer: *mut c_void,
    p_resolve_image_info: *const c_void,
) {
    vkCmdResolveImage2(command_buffer, p_resolve_image_info)
}

pub extern "C" fn vkCmdDrawIndirectCount(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    count_buffer: u64,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|device| {
        let _ = record::cmd_draw_indirect_count(
            device,
            command_buffer,
            buffer,
            offset,
            count_buffer,
            count_buffer_offset,
            max_draw_count,
            stride,
        );
    });
}

pub extern "C" fn vkCmdDrawIndirectCountKHR(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    count_buffer: u64,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    vkCmdDrawIndirectCount(
        command_buffer,
        buffer,
        offset,
        count_buffer,
        count_buffer_offset,
        max_draw_count,
        stride,
    )
}

pub extern "C" fn vkCmdDrawIndirectCountAMD(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    count_buffer: u64,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    vkCmdDrawIndirectCount(
        command_buffer,
        buffer,
        offset,
        count_buffer,
        count_buffer_offset,
        max_draw_count,
        stride,
    )
}

pub extern "C" fn vkCmdDrawIndexedIndirectCount(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    count_buffer: u64,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|device| {
        let _ = record::cmd_draw_indexed_indirect_count(
            device,
            command_buffer,
            buffer,
            offset,
            count_buffer,
            count_buffer_offset,
            max_draw_count,
            stride,
        );
    });
}

pub extern "C" fn vkCmdDrawIndexedIndirectCountKHR(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    count_buffer: u64,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    vkCmdDrawIndexedIndirectCount(
        command_buffer,
        buffer,
        offset,
        count_buffer,
        count_buffer_offset,
        max_draw_count,
        stride,
    )
}

pub extern "C" fn vkCmdDrawIndexedIndirectCountAMD(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    count_buffer: u64,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    vkCmdDrawIndexedIndirectCount(
        command_buffer,
        buffer,
        offset,
        count_buffer,
        count_buffer_offset,
        max_draw_count,
        stride,
    )
}

pub extern "C" fn vkTrimCommandPoolKHR(_device: *mut c_void, _command_pool: u64, _flags: u32) {}
