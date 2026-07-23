use super::*;

// ==================================================================================================
// dynamic state (viewport / scissor lower to IR; the rest is recorded command state)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdSetViewport(
    command_buffer: *mut c_void,
    _first_viewport: u32,
    viewport_count: u32,
    p_viewports: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_viewports.is_null() || viewport_count == 0 {
        return;
    }
    let v = unsafe { &*(p_viewports as *const VkViewport) };
    ShimState::with_device(|d| {
        let _ =
            record::cmd_set_viewport(d, cb, v.x, v.y, v.width, v.height, v.min_depth, v.max_depth);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetScissor(
    command_buffer: *mut c_void,
    _first_scissor: u32,
    scissor_count: u32,
    p_scissors: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_scissors.is_null() || scissor_count == 0 {
        return;
    }
    let r = unsafe { &*(p_scissors as *const VkRect2D) };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_scissor(
            d,
            cb,
            r.offset.x.max(0) as u32,
            r.offset.y.max(0) as u32,
            r.extent.width,
            r.extent.height,
        );
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetLineWidth(command_buffer: *mut c_void, line_width: f32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_line_width(d, cb, line_width);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBias(
    command_buffer: *mut c_void,
    depth_bias_constant_factor: f32,
    depth_bias_clamp: f32,
    depth_bias_slope_factor: f32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_depth_bias(
            d,
            cb,
            depth_bias_constant_factor,
            depth_bias_clamp,
            depth_bias_slope_factor,
        );
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetBlendConstants(command_buffer: *mut c_void, blend_constants: *const f32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if blend_constants.is_null() {
        return;
    }
    let c = unsafe { std::slice::from_raw_parts(blend_constants, 4) };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_blend_constants(d, cb, [c[0], c[1], c[2], c[3]]);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilCompareMask(
    command_buffer: *mut c_void,
    face_mask: u32,
    compare_mask: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_stencil_compare_mask(d, cb, face_mask, compare_mask);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilWriteMask(
    command_buffer: *mut c_void,
    face_mask: u32,
    write_mask: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_stencil_write_mask(d, cb, face_mask, write_mask);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilReference(
    command_buffer: *mut c_void,
    face_mask: u32,
    reference: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_set_stencil_reference(d, cb, face_mask, reference);
    });
}

// ==================================================================================================
// push constants
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdPushConstants(
    command_buffer: *mut c_void,
    _layout: u64,
    _stage_flags: u32,
    offset: u32,
    size: u32,
    p_values: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_values.is_null() || size == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p_values as *const u8, size as usize) };
    ShimState::with_device(|d| {
        let _ = record::cmd_push_constants(d, cb, offset, bytes);
    });
}
