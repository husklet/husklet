use core::ffi::c_void;

use hl_vulkan::service::record;

use super::support::{CommandBuffer, DynamicState, ShimState};

pub extern "C" fn vkCmdSetDepthTestEnable(command_buffer: *mut c_void, depth_test_enable: u32) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_test_enable = depth_test_enable != 0
    });
}

pub extern "C" fn vkCmdSetDepthTestEnableEXT(command_buffer: *mut c_void, depth_test_enable: u32) {
    vkCmdSetDepthTestEnable(command_buffer, depth_test_enable)
}

pub extern "C" fn vkCmdSetDepthWriteEnable(command_buffer: *mut c_void, depth_write_enable: u32) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_write_enable = depth_write_enable != 0
    });
}

pub extern "C" fn vkCmdSetDepthWriteEnableEXT(
    command_buffer: *mut c_void,
    depth_write_enable: u32,
) {
    vkCmdSetDepthWriteEnable(command_buffer, depth_write_enable)
}

pub extern "C" fn vkCmdSetDepthCompareOp(command_buffer: *mut c_void, depth_compare_op: i32) {
    DynamicState::record(command_buffer, |ds| ds.depth_compare_op = depth_compare_op);
}

pub extern "C" fn vkCmdSetDepthCompareOpEXT(command_buffer: *mut c_void, depth_compare_op: i32) {
    vkCmdSetDepthCompareOp(command_buffer, depth_compare_op)
}

pub extern "C" fn vkCmdSetDepthBoundsTestEnable(
    command_buffer: *mut c_void,
    depth_bounds_test_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_bounds_test_enable = depth_bounds_test_enable != 0
    });
}

pub extern "C" fn vkCmdSetDepthBoundsTestEnableEXT(
    command_buffer: *mut c_void,
    depth_bounds_test_enable: u32,
) {
    vkCmdSetDepthBoundsTestEnable(command_buffer, depth_bounds_test_enable)
}

pub extern "C" fn vkCmdSetStencilTestEnable(command_buffer: *mut c_void, stencil_test_enable: u32) {
    DynamicState::record(command_buffer, |ds| {
        ds.stencil_test_enable = stencil_test_enable != 0
    });
}

pub extern "C" fn vkCmdSetStencilTestEnableEXT(
    command_buffer: *mut c_void,
    stencil_test_enable: u32,
) {
    vkCmdSetStencilTestEnable(command_buffer, stencil_test_enable)
}

pub extern "C" fn vkCmdSetStencilOp(
    command_buffer: *mut c_void,
    face_mask: u32,
    fail_op: i32,
    pass_op: i32,
    depth_fail_op: i32,
    compare_op: i32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::set_stencil_op(
            d,
            cb,
            face_mask,
            (fail_op, pass_op, depth_fail_op, compare_op),
        );
    });
}

pub extern "C" fn vkCmdSetStencilOpEXT(
    command_buffer: *mut c_void,
    face_mask: u32,
    fail_op: i32,
    pass_op: i32,
    depth_fail_op: i32,
    compare_op: i32,
) {
    vkCmdSetStencilOp(
        command_buffer,
        face_mask,
        fail_op,
        pass_op,
        depth_fail_op,
        compare_op,
    )
}

pub extern "C" fn vkCmdSetDepthBounds(
    command_buffer: *mut c_void,
    min_depth_bounds: f32,
    max_depth_bounds: f32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.depth_bounds = (min_depth_bounds, max_depth_bounds)
    });
}
