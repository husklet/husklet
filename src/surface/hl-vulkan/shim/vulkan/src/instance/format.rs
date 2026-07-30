use super::*;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{CommandSink, FeatureRequest, WIRE_VERSION};

fn host_supports(format: i32) -> bool {
    let Some(wire) = hl_vulkan::model::memory::Format(format as u32).wire() else {
        return false;
    };
    if wire.block_geometry().is_none() {
        return true;
    }
    StateStore::with(|state| {
        state
            .sink
            .negotiate(&FeatureRequest {
                wire_version: WIRE_VERSION,
                texture_formats: TextureFormat::bits(&[wire]),
                ..FeatureRequest::default()
            })
            .is_ok()
    })
}

/// color-attachment/blend/sampled/storage/blit/transfer; depth: depth-stencil-attachment/sampled/
/// transfer; vertex float: vertex-buffer). Sourced from [`capability::format_features`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties(
    _physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_format_properties as *mut VkFormatProperties).as_mut() }) else {
        return;
    };
    let ff = if host_supports(format) {
        Format(format as u32).features()
    } else {
        Default::default()
    };
    out.linear_tiling_features = ff.linear_tiling;
    out.optimal_tiling_features = ff.optimal_tiling;
    out.buffer_features = ff.buffer;
}

/// `vkGetPhysicalDeviceImageFormatProperties` — the creation limits for a `(format, type, tiling, …)`
/// combination, or `VK_ERROR_FORMAT_NOT_SUPPORTED` when not creatable (spec §12.3). Reports the
/// supported 2D-optimal color subset with the device limits; everything else is truthfully unsupported.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties(
    _physical_device: *mut c_void,
    format: i32,
    image_type: i32,
    tiling: i32,
    _usage: VkFlags,
    _flags: VkFlags,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    const VK_ERROR_FORMAT_NOT_SUPPORTED: VkResult = -11;
    const VK_IMAGE_TYPE_2D: i32 = 1;
    const VK_IMAGE_TILING_OPTIMAL: i32 = 0;
    let Some(out) =
        (unsafe { (p_image_format_properties as *mut VkImageFormatProperties).as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !host_supports(format)
        || !Format(format as u32).is_image_supported()
        || image_type != VK_IMAGE_TYPE_2D
        || tiling != VK_IMAGE_TILING_OPTIMAL
    {
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    let dim = StateStore::with(|s| s.physical_device().limits.max_image_dimension_2d);
    *out = VkImageFormatProperties {
        max_extent: VkExtent3D {
            width: dim,
            height: dim,
            depth: 1,
        },
        max_mip_levels: 1 + (dim as f32).log2() as u32,
        max_array_layers: 2048,
        sample_counts: 0x1 | 0x4,   // TYPE_1 | TYPE_4
        max_resource_size: 1 << 31, // 2 GiB (the executor residency budget)
    };
    VK_SUCCESS
}

// ==================================================================================================
// the `...2` physical-device queries (VK_KHR_get_physical_device_properties2 / core 1.1)
// ==================================================================================================

/// `vkGetPhysicalDeviceProperties2` — the base properties + the pNext payloads apps read back
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties2(
    physical_device: *mut c_void,
    p_image_format_info: *const c_void,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    let Some(info) =
        (unsafe { (p_image_format_info as *const VkPhysicalDeviceImageFormatInfo2).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some(out) =
        (unsafe { (p_image_format_properties as *mut VkImageFormatProperties2).as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    vkGetPhysicalDeviceImageFormatProperties(
        physical_device,
        info.format,
        info.image_type,
        info.tiling,
        info.usage,
        info.flags,
        &mut out.image_format_properties as *mut _ as *mut c_void,
    )
}

/// `vkGetPhysicalDeviceImageFormatProperties2KHR` — the `VK_KHR_get_physical_device_properties2` alias.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties2KHR(
    physical_device: *mut c_void,
    p_image_format_info: *const c_void,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    vkGetPhysicalDeviceImageFormatProperties2(
        physical_device,
        p_image_format_info,
        p_image_format_properties,
    )
}
