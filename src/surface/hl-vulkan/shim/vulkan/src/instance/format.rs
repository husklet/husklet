use super::*;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{CommandSink, FeatureRequest, WIRE_VERSION};
use hl_vulkan::model::capability::format_feature;

use crate::state::State;

/// Which formats this ICD may advertise. The answer depends on the host sink the state owns: a
/// block-compressed or integer colour format is materialised by the host executor and is advertised only
/// if the sink negotiates it, while every other known format is produced by the driver itself. An
/// unrecognised `VkFormat` is never advertised.
impl State {
    fn advertises(&mut self, format: Format) -> bool {
        let Some(wire) = format.wire() else {
            return false;
        };
        // Integer colour joins the negotiated set for the reason `hl_gpu`'s `INTEGER_FORMATS` states: a
        // backend carries raw integer texels only if it really can, and the software oracle — whose
        // clear/blend/sample paths are all defined on normalised float channels — cannot.
        let negotiated = wire.block_geometry().is_some()
            || hl_gpu::protocol::model::capability::INTEGER_FORMATS.contains(&wire);
        if !negotiated {
            return true;
        }
        self.sink
            .negotiate(&FeatureRequest {
                wire_version: WIRE_VERSION,
                texture_formats: TextureFormat::bits(&[wire]),
                ..FeatureRequest::default()
            })
            .is_ok()
    }
}

/// color-attachment/blend/sampled/storage/blit/transfer; depth: depth-stencil-attachment/sampled/
/// transfer; vertex float: vertex-buffer). Sourced from [`capability::format_features`].
pub extern "C" fn vkGetPhysicalDeviceFormatProperties(
    _physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_format_properties as *mut VkFormatProperties).as_mut() }) else {
        return;
    };
    let ff = if StateStore::with(|state| state.advertises(Format(format as u32))) {
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
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties(
    _physical_device: *mut c_void,
    format: i32,
    image_type: i32,
    tiling: i32,
    _usage: VkFlags,
    flags: VkFlags,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    const VK_ERROR_FORMAT_NOT_SUPPORTED: VkResult = -11;
    const VK_IMAGE_TYPE_2D: i32 = 1;
    const VK_IMAGE_TILING_OPTIMAL: i32 = 0;
    const VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT: VkFlags = 0x0000_0010;
    const VK_SAMPLE_COUNT_1_BIT: VkFlags = 0x1;
    const VK_SAMPLE_COUNT_4_BIT: VkFlags = 0x4;
    let Some(out) =
        (unsafe { (p_image_format_properties as *mut VkImageFormatProperties).as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !StateStore::with(|state| state.advertises(Format(format as u32)))
        || !Format(format as u32).is_image_supported()
        || image_type != VK_IMAGE_TYPE_2D
        || tiling != VK_IMAGE_TILING_OPTIMAL
    {
        // "If the combination of parameters ... is not supported by the implementation for use in
        // vkCreateImage, then all members of VkImageFormatProperties will be filled with zero." The
        // refusal used to return without touching the structure at all, which is not the same thing as
        // zeroing it: this is an output the caller need not initialise, so leaving it alone hands back
        // whatever its stack held. That is what 32 dEQP-VK.api.info.image_format_properties cases saw
        // when they checked `maxExtent.width == 0` after a refusal.
        *out = VkImageFormatProperties::default();
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    // Multisampling is available only where the specification permits it to be reported. sampleCounts
    // must be exactly VK_SAMPLE_COUNT_1_BIT unless the image is optimally tiled, two-dimensional, not
    // cube-compatible, and its format can be an attachment. Type and tiling are already settled above,
    // so what remains is the cube flag — which this query used to ignore entirely — and whether the
    // format is attachment-capable, read from the SAME feature mask
    // `vkGetPhysicalDeviceFormatProperties` reports, so the two answers cannot disagree.
    //
    // Reporting 4x everywhere was an over-claim, and an over-claim is the worse direction: a caller that
    // believes it can multisample a sampled-only or cube-compatible image finds out somewhere downstream
    // that cannot name this query as the cause.
    let attachment = format_feature::COLOR_ATTACHMENT | format_feature::DEPTH_STENCIL_ATTACHMENT;
    let multisamplable = flags & VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT == 0
        && Format(format as u32).features().optimal_tiling & attachment != 0;
    let sample_counts = if multisamplable {
        VK_SAMPLE_COUNT_1_BIT | VK_SAMPLE_COUNT_4_BIT
    } else {
        VK_SAMPLE_COUNT_1_BIT
    };
    let dim = StateStore::with(|s| s.physical_device().limits.max_image_dimension_2d);
    *out = VkImageFormatProperties {
        max_extent: VkExtent3D {
            width: dim,
            height: dim,
            depth: 1,
        },
        max_mip_levels: 1 + (dim as f32).log2() as u32,
        max_array_layers: 2048,
        sample_counts,
        max_resource_size: 1 << 31, // 2 GiB (the executor residency budget)
    };
    VK_SUCCESS
}

// ==================================================================================================
// the `...2` physical-device queries (VK_KHR_get_physical_device_properties2 / core 1.1)
// ==================================================================================================

/// `vkGetPhysicalDeviceProperties2` — the base properties + the pNext payloads apps read back
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
