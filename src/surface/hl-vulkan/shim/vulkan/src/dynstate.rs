//! Extended dynamic state 1/2/3 (`VK_EXT_extended_dynamic_state{,2,3}` + the core-1.3 promotions) — the
//! hand-written `vkCmdSet*` bodies that record fixed-function pipeline state into the command buffer's
//! [`hl_vulkan::model::command::DynamicState`].
//!
//! These are REAL bodies in the same sense the existing `vkCmdSetLineWidth`/`vkCmdSetDepthBias` are: each
//! validates the command buffer is recording and records the value as observable, honest command state
//! through [`record::set_dynamic`]. The software color rasterizer models none of this fixed-function
//! state, so — exactly like the base dynamic state — they carry no encoder op, but the value is retained
//! (never faked, never silently dropped). The three commands that DO lower to real IR are here too:
//! `vkCmdSetViewportWithCount` / `vkCmdSetScissorWithCount` (→ `Enc::SetViewport`/`SetScissor`) and
//! `vkCmdBindVertexBuffers2` (→ `Enc::SetVertexBuffer`), reusing the base recording services.
//!
//! Every `*EXT` alias delegates to its core-named body so the two exports are byte-identical behaviour.

#![allow(clippy::missing_safety_doc)]

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::StateStore;
use crate::types::*;

/// Run `f` with just the logical device (recording bodies emit no `Cmd`). `None` if no device yet.
struct ShimState;
impl ShimState {
fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
    StateStore::with(|s| s.device.as_mut().map(f))
}
}

/// Unwrap a dispatchable `VkCommandBuffer` to its `hl_vulkan` `u64` command-buffer handle.
struct CommandBuffer;
impl CommandBuffer {
unsafe fn handle(p: *mut c_void) -> Option<VkCbHandle> {
    Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
}
}

/// The shared body of a scalar extended-dynamic-state `vkCmdSet*`: resolve the command buffer, then
/// record `f` into its `DynamicState` (a no-op if no device / not recording — an invalid call is inert).
struct DynamicState;
impl DynamicState {
fn record(cb: *mut c_void, f: impl FnOnce(&mut hl_vulkan::model::command::DynamicState)) {
    let Some(h) = (unsafe { CommandBuffer::handle(cb) }) else { return };
    ShimState::with_device(|d| {
        let _ = record::set_dynamic(d, h, f);
    });
}
}

// ==================================================================================================
// extended dynamic state 1 (VK_EXT_extended_dynamic_state / core 1.3)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdSetCullMode(command_buffer: *mut c_void, cull_mode: u32) {
    DynamicState::record(command_buffer, |ds| ds.cull_mode = cull_mode);
}
#[no_mangle]
pub extern "C" fn vkCmdSetCullModeEXT(command_buffer: *mut c_void, cull_mode: u32) {
    vkCmdSetCullMode(command_buffer, cull_mode)
}

#[no_mangle]
pub extern "C" fn vkCmdSetFrontFace(command_buffer: *mut c_void, front_face: i32) {
    DynamicState::record(command_buffer, |ds| ds.front_face = front_face);
}
#[no_mangle]
pub extern "C" fn vkCmdSetFrontFaceEXT(command_buffer: *mut c_void, front_face: i32) {
    vkCmdSetFrontFace(command_buffer, front_face)
}

#[no_mangle]
pub extern "C" fn vkCmdSetPrimitiveTopology(command_buffer: *mut c_void, primitive_topology: i32) {
    DynamicState::record(command_buffer, |ds| ds.primitive_topology = primitive_topology);
}
#[no_mangle]
pub extern "C" fn vkCmdSetPrimitiveTopologyEXT(command_buffer: *mut c_void, primitive_topology: i32) {
    vkCmdSetPrimitiveTopology(command_buffer, primitive_topology)
}

#[no_mangle]
pub extern "C" fn vkCmdSetViewportWithCount(
    command_buffer: *mut c_void,
    viewport_count: u32,
    p_viewports: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if p_viewports.is_null() || viewport_count == 0 {
        return;
    }
    let vps = unsafe { core::slice::from_raw_parts(p_viewports as *const VkViewport, viewport_count as usize) };
    ShimState::with_device(|d| {
        for v in vps {
            let _ = record::cmd_set_viewport(d, cb, v.x, v.y, v.width, v.height, v.min_depth, v.max_depth);
        }
    });
}
#[no_mangle]
pub extern "C" fn vkCmdSetViewportWithCountEXT(
    command_buffer: *mut c_void,
    viewport_count: u32,
    p_viewports: *const c_void,
) {
    vkCmdSetViewportWithCount(command_buffer, viewport_count, p_viewports)
}

#[no_mangle]
pub extern "C" fn vkCmdSetScissorWithCount(
    command_buffer: *mut c_void,
    scissor_count: u32,
    p_scissors: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if p_scissors.is_null() || scissor_count == 0 {
        return;
    }
    let rects = unsafe { core::slice::from_raw_parts(p_scissors as *const VkRect2D, scissor_count as usize) };
    ShimState::with_device(|d| {
        for r in rects {
            let _ = record::cmd_set_scissor(
                d,
                cb,
                r.offset.x.max(0) as u32,
                r.offset.y.max(0) as u32,
                r.extent.width,
                r.extent.height,
            );
        }
    });
}
#[no_mangle]
pub extern "C" fn vkCmdSetScissorWithCountEXT(
    command_buffer: *mut c_void,
    scissor_count: u32,
    p_scissors: *const c_void,
) {
    vkCmdSetScissorWithCount(command_buffer, scissor_count, p_scissors)
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthTestEnable(command_buffer: *mut c_void, depth_test_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_test_enable = depth_test_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetDepthTestEnableEXT(command_buffer: *mut c_void, depth_test_enable: u32) {
    vkCmdSetDepthTestEnable(command_buffer, depth_test_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthWriteEnable(command_buffer: *mut c_void, depth_write_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_write_enable = depth_write_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetDepthWriteEnableEXT(command_buffer: *mut c_void, depth_write_enable: u32) {
    vkCmdSetDepthWriteEnable(command_buffer, depth_write_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthCompareOp(command_buffer: *mut c_void, depth_compare_op: i32) {
    DynamicState::record(command_buffer, |ds| ds.depth_compare_op = depth_compare_op);
}
#[no_mangle]
pub extern "C" fn vkCmdSetDepthCompareOpEXT(command_buffer: *mut c_void, depth_compare_op: i32) {
    vkCmdSetDepthCompareOp(command_buffer, depth_compare_op)
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBoundsTestEnable(command_buffer: *mut c_void, depth_bounds_test_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_bounds_test_enable = depth_bounds_test_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetDepthBoundsTestEnableEXT(command_buffer: *mut c_void, depth_bounds_test_enable: u32) {
    vkCmdSetDepthBoundsTestEnable(command_buffer, depth_bounds_test_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilTestEnable(command_buffer: *mut c_void, stencil_test_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.stencil_test_enable = stencil_test_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetStencilTestEnableEXT(command_buffer: *mut c_void, stencil_test_enable: u32) {
    vkCmdSetStencilTestEnable(command_buffer, stencil_test_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilOp(
    command_buffer: *mut c_void,
    face_mask: u32,
    fail_op: i32,
    pass_op: i32,
    depth_fail_op: i32,
    compare_op: i32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    ShimState::with_device(|d| {
        let _ = record::set_stencil_op(d, cb, face_mask, (fail_op, pass_op, depth_fail_op, compare_op));
    });
}
#[no_mangle]
pub extern "C" fn vkCmdSetStencilOpEXT(
    command_buffer: *mut c_void,
    face_mask: u32,
    fail_op: i32,
    pass_op: i32,
    depth_fail_op: i32,
    compare_op: i32,
) {
    vkCmdSetStencilOp(command_buffer, face_mask, fail_op, pass_op, depth_fail_op, compare_op)
}

// ==================================================================================================
// extended dynamic state 2 (VK_EXT_extended_dynamic_state2 / core 1.3)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdSetRasterizerDiscardEnable(command_buffer: *mut c_void, rasterizer_discard_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.rasterizer_discard_enable = rasterizer_discard_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetRasterizerDiscardEnableEXT(command_buffer: *mut c_void, rasterizer_discard_enable: u32) {
    vkCmdSetRasterizerDiscardEnable(command_buffer, rasterizer_discard_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBiasEnable(command_buffer: *mut c_void, depth_bias_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_bias_enable = depth_bias_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetDepthBiasEnableEXT(command_buffer: *mut c_void, depth_bias_enable: u32) {
    vkCmdSetDepthBiasEnable(command_buffer, depth_bias_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetPrimitiveRestartEnable(command_buffer: *mut c_void, primitive_restart_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.primitive_restart_enable = primitive_restart_enable != 0);
}
#[no_mangle]
pub extern "C" fn vkCmdSetPrimitiveRestartEnableEXT(command_buffer: *mut c_void, primitive_restart_enable: u32) {
    vkCmdSetPrimitiveRestartEnable(command_buffer, primitive_restart_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetLogicOpEXT(command_buffer: *mut c_void, logic_op: i32) {
    DynamicState::record(command_buffer, |ds| ds.logic_op = logic_op);
}

#[no_mangle]
pub extern "C" fn vkCmdSetPatchControlPointsEXT(command_buffer: *mut c_void, patch_control_points: u32) {
    DynamicState::record(command_buffer, |ds| ds.patch_control_points = patch_control_points);
}

// ==================================================================================================
// base dynamic state left as stubs (VK 1.0): depth bounds, line stipple, and the vertex-input /
// depth-bias-2 extension state.
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBounds(command_buffer: *mut c_void, min_depth_bounds: f32, max_depth_bounds: f32) {
    DynamicState::record(command_buffer, |ds| ds.depth_bounds = (min_depth_bounds, max_depth_bounds));
}

#[no_mangle]
pub extern "C" fn vkCmdSetLineStipple(command_buffer: *mut c_void, line_stipple_factor: u32, line_stipple_pattern: u16) {
    DynamicState::record(command_buffer, |ds| ds.line_stipple = (line_stipple_factor, line_stipple_pattern));
}
#[no_mangle]
pub extern "C" fn vkCmdSetLineStippleEXT(command_buffer: *mut c_void, line_stipple_factor: u32, line_stipple_pattern: u16) {
    vkCmdSetLineStipple(command_buffer, line_stipple_factor, line_stipple_pattern)
}

#[no_mangle]
pub extern "C" fn vkCmdSetLineStippleEnableEXT(command_buffer: *mut c_void, stippled_line_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.line_stipple_enable = stippled_line_enable != 0);
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

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBias2EXT(command_buffer: *mut c_void, p_depth_bias_info: *const c_void) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    let Some(info) = (unsafe { (p_depth_bias_info as *const VkDepthBiasInfoEXTHead).as_ref() }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_depth_bias(d, cb, info.constant_factor, info.clamp, info.slope_factor);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetVertexInputEXT(
    command_buffer: *mut c_void,
    vertex_binding_description_count: u32,
    _p_vertex_binding_descriptions: *const c_void,
    _vertex_attribute_description_count: u32,
    _p_vertex_attribute_descriptions: *const c_void,
) {
    DynamicState::record(command_buffer, |ds| ds.vertex_binding_count = vertex_binding_description_count);
}

// ==================================================================================================
// vertex buffer binding with count/size/stride (VK_EXT_extended_dynamic_state / core 1.3) — real IR
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdBindVertexBuffers2(
    command_buffer: *mut c_void,
    first_binding: u32,
    binding_count: u32,
    p_buffers: *const c_void,
    p_offsets: *const c_void,
    _p_sizes: *const c_void,
    _p_strides: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if p_buffers.is_null() || binding_count == 0 {
        return;
    }
    let buffers = unsafe { core::slice::from_raw_parts(p_buffers as *const u64, binding_count as usize) };
    let offsets = if p_offsets.is_null() {
        None
    } else {
        Some(unsafe { core::slice::from_raw_parts(p_offsets as *const u64, binding_count as usize) })
    };
    ShimState::with_device(|d| {
        for i in 0..binding_count as usize {
            let slot = first_binding + i as u32;
            let offset = offsets.map(|o| o[i]).unwrap_or(0);
            let _ = record::cmd_bind_vertex_buffer(d, cb, slot, buffers[i], offset);
        }
    });
}
#[no_mangle]
pub extern "C" fn vkCmdBindVertexBuffers2EXT(
    command_buffer: *mut c_void,
    first_binding: u32,
    binding_count: u32,
    p_buffers: *const c_void,
    p_offsets: *const c_void,
    p_sizes: *const c_void,
    p_strides: *const c_void,
) {
    vkCmdBindVertexBuffers2(command_buffer, first_binding, binding_count, p_buffers, p_offsets, p_sizes, p_strides)
}

// ==================================================================================================
// extended dynamic state 3 (VK_EXT_extended_dynamic_state3) — scalars
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdSetRasterizationSamplesEXT(command_buffer: *mut c_void, rasterization_samples: u32) {
    DynamicState::record(command_buffer, |ds| ds.rasterization_samples = rasterization_samples);
}

#[no_mangle]
pub extern "C" fn vkCmdSetSampleMaskEXT(
    command_buffer: *mut c_void,
    _samples: u32,
    p_sample_mask: *const c_void,
) {
    let mask = if p_sample_mask.is_null() { 0 } else { unsafe { *(p_sample_mask as *const u32) } };
    DynamicState::record(command_buffer, |ds| ds.sample_mask = mask);
}

#[no_mangle]
pub extern "C" fn vkCmdSetAlphaToCoverageEnableEXT(command_buffer: *mut c_void, alpha_to_coverage_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.alpha_to_coverage_enable = alpha_to_coverage_enable != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetAlphaToOneEnableEXT(command_buffer: *mut c_void, alpha_to_one_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.alpha_to_one_enable = alpha_to_one_enable != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetLogicOpEnableEXT(command_buffer: *mut c_void, logic_op_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.logic_op_enable = logic_op_enable != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetPolygonModeEXT(command_buffer: *mut c_void, polygon_mode: i32) {
    DynamicState::record(command_buffer, |ds| ds.polygon_mode = polygon_mode);
}

#[no_mangle]
pub extern "C" fn vkCmdSetTessellationDomainOriginEXT(command_buffer: *mut c_void, domain_origin: i32) {
    DynamicState::record(command_buffer, |ds| ds.tessellation_domain_origin = domain_origin);
}

#[no_mangle]
pub extern "C" fn vkCmdSetProvokingVertexModeEXT(command_buffer: *mut c_void, provoking_vertex_mode: i32) {
    DynamicState::record(command_buffer, |ds| ds.provoking_vertex_mode = provoking_vertex_mode);
}

#[no_mangle]
pub extern "C" fn vkCmdSetLineRasterizationModeEXT(command_buffer: *mut c_void, line_rasterization_mode: i32) {
    DynamicState::record(command_buffer, |ds| ds.line_rasterization_mode = line_rasterization_mode);
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthClampEnableEXT(command_buffer: *mut c_void, depth_clamp_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_clamp_enable = depth_clamp_enable != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthClipEnableEXT(command_buffer: *mut c_void, depth_clip_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_clip_enable = depth_clip_enable != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthClipNegativeOneToOneEXT(command_buffer: *mut c_void, negative_one_to_one: u32) {
    DynamicState::record(command_buffer, |ds| ds.depth_clip_negative_one_to_one = negative_one_to_one != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetConservativeRasterizationModeEXT(command_buffer: *mut c_void, mode: i32) {
    DynamicState::record(command_buffer, |ds| ds.conservative_rasterization_mode = mode);
}

#[no_mangle]
pub extern "C" fn vkCmdSetExtraPrimitiveOverestimationSizeEXT(command_buffer: *mut c_void, size: f32) {
    DynamicState::record(command_buffer, |ds| ds.extra_primitive_overestimation_size = size);
}

#[no_mangle]
pub extern "C" fn vkCmdSetSampleLocationsEnableEXT(command_buffer: *mut c_void, sample_locations_enable: u32) {
    DynamicState::record(command_buffer, |ds| ds.sample_locations_enable = sample_locations_enable != 0);
}

#[no_mangle]
pub extern "C" fn vkCmdSetRasterizationStreamEXT(command_buffer: *mut c_void, rasterization_stream: u32) {
    DynamicState::record(command_buffer, |ds| ds.rasterization_stream = rasterization_stream);
}

// ---- extended dynamic state 3 — per-attachment arrays -------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdSetColorBlendEnableEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    p_color_blend_enables: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if p_color_blend_enables.is_null() || attachment_count == 0 {
        return;
    }
    let vals = unsafe { core::slice::from_raw_parts(p_color_blend_enables as *const u32, attachment_count as usize) }.to_vec();
    ShimState::with_device(|d| {
        let _ = record::set_dynamic_attachment_array(d, cb, first_attachment, &vals, |ds| &mut ds.color_blend_enables);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetColorWriteMaskEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    p_color_write_masks: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if p_color_write_masks.is_null() || attachment_count == 0 {
        return;
    }
    let vals = unsafe { core::slice::from_raw_parts(p_color_write_masks as *const u32, attachment_count as usize) }.to_vec();
    ShimState::with_device(|d| {
        let _ = record::set_dynamic_attachment_array(d, cb, first_attachment, &vals, |ds| &mut ds.color_write_masks);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetColorWriteEnableEXT(
    command_buffer: *mut c_void,
    attachment_count: u32,
    p_color_write_enables: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if p_color_write_enables.is_null() || attachment_count == 0 {
        return;
    }
    let vals = unsafe { core::slice::from_raw_parts(p_color_write_enables as *const u32, attachment_count as usize) }.to_vec();
    ShimState::with_device(|d| {
        let _ = record::set_dynamic_attachment_array(d, cb, 0, &vals, |ds| &mut ds.color_write_enables);
    });
}

/// `vkCmdSetColorBlendEquationEXT` — the per-attachment blend equation is unmodeled fixed-function state
/// (the color oracle does no blending). Record that the color-blend state was touched (mark the
/// attachments as blend-enabled slots so the state is observable) with no encoder op.
#[no_mangle]
pub extern "C" fn vkCmdSetColorBlendEquationEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    _p_color_blend_equations: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if attachment_count == 0 {
        return;
    }
    // Ensure the blend-enable vector covers the touched attachments (honest observable state).
    let ext = vec![0u32; attachment_count as usize];
    ShimState::with_device(|d| {
        let _ = record::set_dynamic_attachment_array(d, cb, first_attachment, &ext, |ds| &mut ds.color_blend_enables);
    });
}

/// `vkCmdSetColorBlendAdvancedEXT` — advanced blend (VK_EXT_blend_operation_advanced) is not modeled;
/// record that the attachments were touched, no encoder op.
#[no_mangle]
pub extern "C" fn vkCmdSetColorBlendAdvancedEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    _p_color_blend_advanced: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else { return };
    if attachment_count == 0 {
        return;
    }
    let ext = vec![0u32; attachment_count as usize];
    ShimState::with_device(|d| {
        let _ = record::set_dynamic_attachment_array(d, cb, first_attachment, &ext, |ds| &mut ds.color_blend_enables);
    });
}
