use core::ffi::c_void;

use hl_vulkan::service::record;

use super::support::{CommandBuffer, DynamicState, ShimState};

pub extern "C" fn vkCmdSetRasterizerDiscardEnable(
    command_buffer: *mut c_void,
    rasterizer_discard_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.rasterizer_discard_enable = rasterizer_discard_enable != 0
    });
}

pub extern "C" fn vkCmdSetRasterizerDiscardEnableEXT(
    command_buffer: *mut c_void,
    rasterizer_discard_enable: u32,
) {
    vkCmdSetRasterizerDiscardEnable(command_buffer, rasterizer_discard_enable)
}

pub extern "C" fn vkCmdSetDepthBiasEnable(command_buffer: *mut c_void, depth_bias_enable: u32) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_bias_enable = depth_bias_enable != 0
    });
}

pub extern "C" fn vkCmdSetDepthBiasEnableEXT(command_buffer: *mut c_void, depth_bias_enable: u32) {
    vkCmdSetDepthBiasEnable(command_buffer, depth_bias_enable)
}

pub extern "C" fn vkCmdSetLineStipple(
    command_buffer: *mut c_void,
    line_stipple_factor: u32,
    line_stipple_pattern: u16,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.line_stipple = (line_stipple_factor, line_stipple_pattern)
    });
}

pub extern "C" fn vkCmdSetLineStippleEXT(
    command_buffer: *mut c_void,
    line_stipple_factor: u32,
    line_stipple_pattern: u16,
) {
    vkCmdSetLineStipple(command_buffer, line_stipple_factor, line_stipple_pattern)
}

pub extern "C" fn vkCmdSetLineStippleEnableEXT(
    command_buffer: *mut c_void,
    stippled_line_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.line_stipple_enable = stippled_line_enable != 0
    });
}

/// `VkDepthBiasInfoEXT` head: `sType`, `pNext`, then `depthBiasConstantFactor`, `depthBiasClamp`,
/// `depthBiasSlopeFactor` (three `f32`). We read the three factors and record them as the base depth bias.
#[repr(C)]
struct VkDepthBiasInfoEXTHead {
    s_type: i32,
    _pad: u32,
    p_next: *const c_void,
    constant_factor: f32,
    clamp: f32,
    slope_factor: f32,
}

pub extern "C" fn vkCmdSetDepthBias2EXT(
    command_buffer: *mut c_void,
    p_depth_bias_info: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(info) = (unsafe { (p_depth_bias_info as *const VkDepthBiasInfoEXTHead).as_ref() })
    else {
        return;
    };
    ShimState::with_device(|d| {
        let _ =
            record::cmd_set_depth_bias(d, cb, info.constant_factor, info.clamp, info.slope_factor);
    });
}

pub extern "C" fn vkCmdSetPolygonModeEXT(command_buffer: *mut c_void, polygon_mode: i32) {
    DynamicState::record(command_buffer, |ds| ds.polygon_mode = polygon_mode);
}

pub extern "C" fn vkCmdSetLineRasterizationModeEXT(
    command_buffer: *mut c_void,
    line_rasterization_mode: i32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.line_rasterization_mode = line_rasterization_mode
    });
}

pub extern "C" fn vkCmdSetDepthClampEnableEXT(
    command_buffer: *mut c_void,
    depth_clamp_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_clamp_enable = depth_clamp_enable != 0
    });
}

pub extern "C" fn vkCmdSetDepthClipEnableEXT(command_buffer: *mut c_void, depth_clip_enable: u32) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_clip_enable = depth_clip_enable != 0
    });
}

pub extern "C" fn vkCmdSetDepthClipNegativeOneToOneEXT(
    command_buffer: *mut c_void,
    negative_one_to_one: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_clip_negative_one_to_one = negative_one_to_one != 0
    });
}

pub extern "C" fn vkCmdSetConservativeRasterizationModeEXT(command_buffer: *mut c_void, mode: i32) {
    DynamicState::record(command_buffer, |ds| {
        ds.conservative_rasterization_mode = mode
    });
}

pub extern "C" fn vkCmdSetExtraPrimitiveOverestimationSizeEXT(
    command_buffer: *mut c_void,
    size: f32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.extra_primitive_overestimation_size = size
    });
}

pub extern "C" fn vkCmdSetRasterizationStreamEXT(
    command_buffer: *mut c_void,
    rasterization_stream: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.rasterization_stream = rasterization_stream
    });
}
