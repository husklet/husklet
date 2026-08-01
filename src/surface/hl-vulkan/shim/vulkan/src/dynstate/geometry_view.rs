use core::ffi::c_void;

use hl_vulkan::service::record;

use crate::types::*;

use super::support::{CommandBuffer, DynamicState, ShimState};

pub extern "C" fn vkCmdSetCullMode(command_buffer: *mut c_void, cull_mode: u32) {
    DynamicState::record(command_buffer, |ds| ds.cull_mode = cull_mode);
}

pub extern "C" fn vkCmdSetCullModeEXT(command_buffer: *mut c_void, cull_mode: u32) {
    vkCmdSetCullMode(command_buffer, cull_mode)
}

pub extern "C" fn vkCmdSetFrontFace(command_buffer: *mut c_void, front_face: i32) {
    DynamicState::record(command_buffer, |ds| ds.front_face = front_face);
}

pub extern "C" fn vkCmdSetFrontFaceEXT(command_buffer: *mut c_void, front_face: i32) {
    vkCmdSetFrontFace(command_buffer, front_face)
}

pub extern "C" fn vkCmdSetPrimitiveTopology(command_buffer: *mut c_void, primitive_topology: i32) {
    DynamicState::record(command_buffer, |ds| {
        ds.primitive_topology = primitive_topology
    });
}

pub extern "C" fn vkCmdSetPrimitiveTopologyEXT(
    command_buffer: *mut c_void,
    primitive_topology: i32,
) {
    vkCmdSetPrimitiveTopology(command_buffer, primitive_topology)
}

pub extern "C" fn vkCmdSetViewportWithCount(
    command_buffer: *mut c_void,
    viewport_count: u32,
    p_viewports: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_viewports.is_null() || viewport_count == 0 {
        return;
    }
    let vps = unsafe {
        core::slice::from_raw_parts(p_viewports as *const VkViewport, viewport_count as usize)
    };
    ShimState::with_device(|d| {
        for v in vps {
            let recorded = record::cmd_set_viewport(
                d,
                cb,
                v.x,
                v.y,
                v.width,
                v.height,
                v.min_depth,
                v.max_depth,
            );
            d.latch(cb, recorded);
        }
    });
}

pub extern "C" fn vkCmdSetViewportWithCountEXT(
    command_buffer: *mut c_void,
    viewport_count: u32,
    p_viewports: *const c_void,
) {
    vkCmdSetViewportWithCount(command_buffer, viewport_count, p_viewports)
}

pub extern "C" fn vkCmdSetScissorWithCount(
    command_buffer: *mut c_void,
    scissor_count: u32,
    p_scissors: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_scissors.is_null() || scissor_count == 0 {
        return;
    }
    let rects = unsafe {
        core::slice::from_raw_parts(p_scissors as *const VkRect2D, scissor_count as usize)
    };
    ShimState::with_device(|d| {
        for r in rects {
            let recorded = record::cmd_set_scissor(
                d,
                cb,
                r.offset.x.max(0) as u32,
                r.offset.y.max(0) as u32,
                r.extent.width,
                r.extent.height,
            );
            d.latch(cb, recorded);
        }
    });
}

pub extern "C" fn vkCmdSetScissorWithCountEXT(
    command_buffer: *mut c_void,
    scissor_count: u32,
    p_scissors: *const c_void,
) {
    vkCmdSetScissorWithCount(command_buffer, scissor_count, p_scissors)
}
