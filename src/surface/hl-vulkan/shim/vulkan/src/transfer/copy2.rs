//! Vulkan 1.3 and `VK_KHR_copy_commands2` transfer commands.

use core::ffi::c_void;

use hl_vulkan::service::record;

use super::{CommandBuffer, ShimState};
use crate::types::*;

struct Transfer2;

impl Transfer2 {
    fn copy_buffer(command_buffer: *mut c_void, info: *const c_void) {
        let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
            return;
        };
        let Some(info) = (unsafe { (info as *const VkCopyBufferInfo2).as_ref() }) else {
            return;
        };
        if info.p_regions.is_null() {
            return;
        }
        let regions =
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
        ShimState::with_device(|device| {
            for region in regions {
                let _ = record::cmd_copy_buffer(
                    device,
                    command_buffer,
                    info.src_buffer,
                    info.dst_buffer,
                    region.src_offset,
                    region.dst_offset,
                    region.size,
                );
            }
        });
    }

    fn copy_buffer_to_image(command_buffer: *mut c_void, info: *const c_void) {
        let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
            return;
        };
        let Some(info) = (unsafe { (info as *const VkCopyBufferToImageInfo2).as_ref() }) else {
            return;
        };
        if info.p_regions.is_null() {
            return;
        }
        let regions =
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
        ShimState::with_device(|device| {
            for region in regions {
                if region.image_subresource.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0
                    || region.image_offset.x < 0
                    || region.image_offset.y < 0
                    || region.image_offset.z < 0
                {
                    continue;
                }
                let _ = record::cmd_copy_buffer_to_image_region(
                    device,
                    command_buffer,
                    info.src_buffer,
                    info.dst_image,
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
            }
        });
    }

    fn copy_image(command_buffer: *mut c_void, info: *const c_void) {
        let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
            return;
        };
        let Some(info) = (unsafe { (info as *const VkCopyImageInfo2).as_ref() }) else {
            return;
        };
        if info.p_regions.is_null() {
            return;
        }
        let regions =
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
        ShimState::with_device(|device| {
            for region in regions {
                if region.src_offset.x < 0
                    || region.src_offset.y < 0
                    || region.dst_offset.x < 0
                    || region.dst_offset.y < 0
                {
                    continue;
                }
                let _ = record::cmd_copy_image(
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

    fn copy_image_to_buffer(command_buffer: *mut c_void, info: *const c_void) {
        let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
            return;
        };
        let Some(info) = (unsafe { (info as *const VkCopyImageToBufferInfo2).as_ref() }) else {
            return;
        };
        if info.p_regions.is_null() {
            return;
        }
        let regions =
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
        ShimState::with_device(|device| {
            for region in regions {
                if region.image_subresource.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0
                    || region.image_offset.x < 0
                    || region.image_offset.y < 0
                    || region.image_offset.z < 0
                {
                    continue;
                }
                let _ = record::cmd_copy_image_to_buffer_region(
                    device,
                    command_buffer,
                    info.src_image,
                    info.dst_buffer,
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
            }
        });
    }

    fn blit_image(command_buffer: *mut c_void, info: *const c_void) {
        let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
            return;
        };
        let Some(info) = (unsafe { (info as *const VkBlitImageInfo2).as_ref() }) else {
            return;
        };
        if info.p_regions.is_null() {
            return;
        }
        let regions =
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
        let linear = info.filter == VK_FILTER_LINEAR;
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
                let _ = record::cmd_blit_image(
                    device,
                    command_buffer,
                    info.src_image,
                    info.dst_image,
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
            }
        });
    }
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBuffer2(command_buffer: *mut c_void, p_copy_buffer_info: *const c_void) {
    Transfer2::copy_buffer(command_buffer, p_copy_buffer_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBuffer2KHR(
    command_buffer: *mut c_void,
    p_copy_buffer_info: *const c_void,
) {
    Transfer2::copy_buffer(command_buffer, p_copy_buffer_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBufferToImage2(
    command_buffer: *mut c_void,
    p_copy_buffer_to_image_info: *const c_void,
) {
    Transfer2::copy_buffer_to_image(command_buffer, p_copy_buffer_to_image_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBufferToImage2KHR(
    command_buffer: *mut c_void,
    p_copy_buffer_to_image_info: *const c_void,
) {
    Transfer2::copy_buffer_to_image(command_buffer, p_copy_buffer_to_image_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImage2(command_buffer: *mut c_void, p_copy_image_info: *const c_void) {
    Transfer2::copy_image(command_buffer, p_copy_image_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImage2KHR(
    command_buffer: *mut c_void,
    p_copy_image_info: *const c_void,
) {
    Transfer2::copy_image(command_buffer, p_copy_image_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImageToBuffer2(
    command_buffer: *mut c_void,
    p_copy_image_to_buffer_info: *const c_void,
) {
    Transfer2::copy_image_to_buffer(command_buffer, p_copy_image_to_buffer_info);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImageToBuffer2KHR(
    command_buffer: *mut c_void,
    p_copy_image_to_buffer_info: *const c_void,
) {
    Transfer2::copy_image_to_buffer(command_buffer, p_copy_image_to_buffer_info);
}

#[no_mangle]
pub extern "C" fn vkCmdBlitImage2(command_buffer: *mut c_void, p_blit_image_info: *const c_void) {
    Transfer2::blit_image(command_buffer, p_blit_image_info);
}

#[no_mangle]
pub extern "C" fn vkCmdBlitImage2KHR(
    command_buffer: *mut c_void,
    p_blit_image_info: *const c_void,
) {
    Transfer2::blit_image(command_buffer, p_blit_image_info);
}
