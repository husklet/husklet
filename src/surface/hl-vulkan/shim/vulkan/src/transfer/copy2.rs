//! Vulkan 1.3 and `VK_KHR_copy_commands2` transfer commands.

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::SubresourceLayers;

use super::{BlitRect, CommandBuffer, ShimState};
use hl_gpu::protocol::model::descriptor::Mirror;
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
                let recorded = record::cmd_copy_buffer(
                    device,
                    command_buffer,
                    info.src_buffer,
                    info.dst_buffer,
                    region.src_offset,
                    region.dst_offset,
                    region.size,
                );
                device.latch(command_buffer, recorded);
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
                let recorded = record::cmd_copy_buffer_to_image_region(
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
                device.latch(command_buffer, recorded);
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
                let recorded = record::cmd_copy_image(
                    device,
                    command_buffer,
                    info.src_image,
                    info.dst_image,
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
                let recorded = record::cmd_copy_image_to_buffer_region(
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
                device.latch(command_buffer, recorded);
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
                // Same normalization as `vkCmdBlitImage`: an empty rect is skipped and an inverted one
                // is a legal MIRROR whose flip is carried into the IR. The `<=` comparison this replaced
                // conflated the two, so every mirrored blit2 was dropped with no error at all.
                let (Some(source), Some(destination)) = (
                    BlitRect::of(&region.src_offsets),
                    BlitRect::of(&region.dst_offsets),
                ) else {
                    continue;
                };
                if source.depth != (0, 1) || destination.depth != (0, 1) {
                    device.latch::<()>(
                        command_buffer,
                        Err(hl_gpu::GpuError::Unsupported("vkCmdBlitImage2: 3D region")),
                    );
                    continue;
                }
                let recorded = record::cmd_blit_image(
                    device,
                    command_buffer,
                    info.src_image,
                    info.dst_image,
                    layers_of(&region.src_subresource),
                    layers_of(&region.dst_subresource),
                    source.origin,
                    source.extent,
                    destination.origin,
                    destination.extent,
                    linear,
                    Mirror::net(source.inverted, destination.inverted),
                );
                device.latch(command_buffer, recorded);
            }
        });
    }
}

pub extern "C" fn vkCmdCopyBuffer2(command_buffer: *mut c_void, p_copy_buffer_info: *const c_void) {
    Transfer2::copy_buffer(command_buffer, p_copy_buffer_info);
}

pub extern "C" fn vkCmdCopyBuffer2KHR(
    command_buffer: *mut c_void,
    p_copy_buffer_info: *const c_void,
) {
    Transfer2::copy_buffer(command_buffer, p_copy_buffer_info);
}

pub extern "C" fn vkCmdCopyBufferToImage2(
    command_buffer: *mut c_void,
    p_copy_buffer_to_image_info: *const c_void,
) {
    Transfer2::copy_buffer_to_image(command_buffer, p_copy_buffer_to_image_info);
}

pub extern "C" fn vkCmdCopyBufferToImage2KHR(
    command_buffer: *mut c_void,
    p_copy_buffer_to_image_info: *const c_void,
) {
    Transfer2::copy_buffer_to_image(command_buffer, p_copy_buffer_to_image_info);
}

pub extern "C" fn vkCmdCopyImage2(command_buffer: *mut c_void, p_copy_image_info: *const c_void) {
    Transfer2::copy_image(command_buffer, p_copy_image_info);
}

pub extern "C" fn vkCmdCopyImage2KHR(
    command_buffer: *mut c_void,
    p_copy_image_info: *const c_void,
) {
    Transfer2::copy_image(command_buffer, p_copy_image_info);
}

pub extern "C" fn vkCmdCopyImageToBuffer2(
    command_buffer: *mut c_void,
    p_copy_image_to_buffer_info: *const c_void,
) {
    Transfer2::copy_image_to_buffer(command_buffer, p_copy_image_to_buffer_info);
}

pub extern "C" fn vkCmdCopyImageToBuffer2KHR(
    command_buffer: *mut c_void,
    p_copy_image_to_buffer_info: *const c_void,
) {
    Transfer2::copy_image_to_buffer(command_buffer, p_copy_image_to_buffer_info);
}

pub extern "C" fn vkCmdBlitImage2(command_buffer: *mut c_void, p_blit_image_info: *const c_void) {
    Transfer2::blit_image(command_buffer, p_blit_image_info);
}

pub extern "C" fn vkCmdBlitImage2KHR(
    command_buffer: *mut c_void,
    p_blit_image_info: *const c_void,
) {
    Transfer2::blit_image(command_buffer, p_blit_image_info);
}
