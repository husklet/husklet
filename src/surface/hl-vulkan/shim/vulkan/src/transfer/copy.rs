//! Vulkan 1.0 copy and blit commands.

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::SubresourceLayers;

use super::{CommandBuffer, ShimState};
use crate::types::*;

/// Translate a `VkImageSubresourceLayers` into the driver's own value. `layerCount` may be
/// `VK_REMAINING_ARRAY_LAYERS`, which the recorder resolves against the image it names.
fn layers_of(sub: &VkImageSubresourceLayers) -> SubresourceLayers {
    SubresourceLayers {
        mip_level: sub.mip_level,
        base_array_layer: sub.base_array_layer,
        layer_count: sub.layer_count,
        // The aspect the region named. Carried rather than dropped: the image-to-image copy path used to
        // discard it and record `TextureAspect::All`, which silently copies both planes of a combined
        // depth/stencil image when the guest asked for one.
        aspect_mask: sub.aspect_mask,
    }
}

pub extern "C" fn vkCmdCopyBuffer(
    command_buffer: *mut c_void,
    src_buffer: u64,
    dst_buffer: u64,
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
        std::slice::from_raw_parts(p_regions as *const VkBufferCopy, region_count as usize)
    };
    ShimState::with_device(|device| {
        for region in regions {
            let recorded = record::cmd_copy_buffer(
                device,
                command_buffer,
                src_buffer,
                dst_buffer,
                region.src_offset,
                region.dst_offset,
                region.size,
            );
            device.latch(command_buffer, recorded);
        }
    });
}

pub extern "C" fn vkCmdCopyBufferToImage(
    command_buffer: *mut c_void,
    src_buffer: u64,
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
        std::slice::from_raw_parts(p_regions as *const VkBufferImageCopy, region_count as usize)
    };
    ShimState::with_device(|device| {
        for region in regions {
            if region.image_subresource.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0
                || region.image_offset.x < 0
                || region.image_offset.y < 0
                || region.image_offset.z < 0
            {
                continue;
            }
            let recorded = record::cmd_copy_buffer_to_image_region(
                device,
                command_buffer,
                src_buffer,
                dst_image,
                region.buffer_offset,
                region.buffer_row_length,
                region.buffer_image_height,
                region.image_subresource.mip_level,
                region.image_subresource.base_array_layer,
                region.image_offset.x as u32,
                region.image_offset.y as u32,
                region.image_offset.z as u32,
                region.image_extent.width,
                region.image_extent.height.max(1),
                region
                    .image_subresource
                    .layer_count
                    .max(region.image_extent.depth.max(1)),
            );
            device.latch(command_buffer, recorded);
        }
    });
}

pub extern "C" fn vkCmdCopyImageToBuffer(
    command_buffer: *mut c_void,
    src_image: u64,
    _src_image_layout: i32,
    dst_buffer: u64,
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
        std::slice::from_raw_parts(p_regions as *const VkBufferImageCopy, region_count as usize)
    };
    ShimState::with_device(|device| {
        for region in regions {
            if region.image_subresource.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0
                || region.image_offset.x < 0
                || region.image_offset.y < 0
                || region.image_offset.z < 0
            {
                continue;
            }
            let recorded = record::cmd_copy_image_to_buffer_region(
                device,
                command_buffer,
                src_image,
                dst_buffer,
                region.buffer_offset,
                region.buffer_row_length,
                region.buffer_image_height,
                region.image_subresource.mip_level,
                region.image_subresource.base_array_layer,
                region.image_offset.x as u32,
                region.image_offset.y as u32,
                region.image_offset.z as u32,
                region.image_extent.width,
                region.image_extent.height.max(1),
                region
                    .image_subresource
                    .layer_count
                    .max(region.image_extent.depth.max(1)),
            );
            device.latch(command_buffer, recorded);
        }
    });
}

pub extern "C" fn vkCmdCopyImage(
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
            let recorded = record::cmd_copy_image(
                device,
                command_buffer,
                src_image,
                dst_image,
                layers_of(&region.src_subresource),
                layers_of(&region.dst_subresource),
                (region.src_offset.x as u32, region.src_offset.y as u32),
                (region.dst_offset.x as u32, region.dst_offset.y as u32),
                (region.extent.width, region.extent.height.max(1)),
            );
            device.latch(command_buffer, recorded);
        }
    });
}

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
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_regions.is_null() {
        return;
    }
    let regions = unsafe {
        std::slice::from_raw_parts(p_regions as *const VkImageBlit, region_count as usize)
    };
    let linear = filter == VK_FILTER_LINEAR;
    ShimState::with_device(|device| {
        for region in regions {
            let [source_start, source_end] = region.src_offsets;
            let [destination_start, destination_end] = region.dst_offsets;
            if source_end.x <= source_start.x
                || source_end.y <= source_start.y
                || source_start.z != 0
                || source_end.z != 1
                || destination_end.x <= destination_start.x
                || destination_end.y <= destination_start.y
                || destination_start.z != 0
                || destination_end.z != 1
            {
                continue;
            }
            let recorded = record::cmd_blit_image(
                device,
                command_buffer,
                src_image,
                dst_image,
                layers_of(&region.src_subresource),
                layers_of(&region.dst_subresource),
                (source_start.x as u32, source_start.y as u32),
                (
                    (source_end.x - source_start.x) as u32,
                    (source_end.y - source_start.y) as u32,
                ),
                (destination_start.x as u32, destination_start.y as u32),
                (
                    (destination_end.x - destination_start.x) as u32,
                    (destination_end.y - destination_start.y) as u32,
                ),
                linear,
            );
            device.latch(command_buffer, recorded);
        }
    });
}
