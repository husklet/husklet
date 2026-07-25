use core::ffi::c_void;

use hl_vulkan::service::record;

use super::support::{CommandBuffer, DynamicState, ShimState};

#[no_mangle]
pub extern "C" fn vkCmdSetPrimitiveRestartEnable(
    command_buffer: *mut c_void,
    primitive_restart_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.primitive_restart_enable = primitive_restart_enable != 0
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetPrimitiveRestartEnableEXT(
    command_buffer: *mut c_void,
    primitive_restart_enable: u32,
) {
    vkCmdSetPrimitiveRestartEnable(command_buffer, primitive_restart_enable)
}

#[no_mangle]
pub extern "C" fn vkCmdSetPatchControlPointsEXT(
    command_buffer: *mut c_void,
    patch_control_points: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.patch_control_points = patch_control_points
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
    DynamicState::record(command_buffer, |ds| {
        ds.vertex_binding_count = vertex_binding_description_count
    });
}

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
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_buffers.is_null() || binding_count == 0 {
        return;
    }
    let buffers =
        unsafe { core::slice::from_raw_parts(p_buffers as *const u64, binding_count as usize) };
    let offsets = if p_offsets.is_null() {
        None
    } else {
        Some(unsafe {
            core::slice::from_raw_parts(p_offsets as *const u64, binding_count as usize)
        })
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
    vkCmdBindVertexBuffers2(
        command_buffer,
        first_binding,
        binding_count,
        p_buffers,
        p_offsets,
        p_sizes,
        p_strides,
    )
}

#[no_mangle]
pub extern "C" fn vkCmdSetTessellationDomainOriginEXT(
    command_buffer: *mut c_void,
    domain_origin: i32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.tessellation_domain_origin = domain_origin
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetProvokingVertexModeEXT(
    command_buffer: *mut c_void,
    provoking_vertex_mode: i32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.provoking_vertex_mode = provoking_vertex_mode
    });
}
