use super::*;

// ==================================================================================================
// dynamic state (viewport / scissor lower to IR; the rest is recorded command state)
// ==================================================================================================

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
        // Latched like every other recording command: the only failure here is `require_recording`,
        // which means the application issued a command outside a recording command buffer, and that
        // is precisely what `vkEndCommandBuffer` exists to report.
        let recorded =
            record::cmd_set_viewport(d, cb, v.x, v.y, v.width, v.height, v.min_depth, v.max_depth);
        d.latch(cb, recorded);
    });
}

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
        let recorded = record::cmd_set_scissor(
            d,
            cb,
            r.offset.x.max(0) as u32,
            r.offset.y.max(0) as u32,
            r.extent.width,
            r.extent.height,
        );
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetLineWidth(command_buffer: *mut c_void, line_width: f32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let recorded = record::cmd_set_line_width(d, cb, line_width);
        d.latch(cb, recorded);
    });
}

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
        let recorded = record::cmd_set_depth_bias(
            d,
            cb,
            depth_bias_constant_factor,
            depth_bias_clamp,
            depth_bias_slope_factor,
        );
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetBlendConstants(command_buffer: *mut c_void, blend_constants: *const f32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if blend_constants.is_null() {
        return;
    }
    let c = unsafe { std::slice::from_raw_parts(blend_constants, 4) };
    ShimState::with_device(|d| {
        let recorded = record::cmd_set_blend_constants(d, cb, [c[0], c[1], c[2], c[3]]);
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetStencilCompareMask(
    command_buffer: *mut c_void,
    face_mask: u32,
    compare_mask: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let recorded = record::cmd_set_stencil_compare_mask(d, cb, face_mask, compare_mask);
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetStencilWriteMask(
    command_buffer: *mut c_void,
    face_mask: u32,
    write_mask: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let recorded = record::cmd_set_stencil_write_mask(d, cb, face_mask, write_mask);
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetStencilReference(
    command_buffer: *mut c_void,
    face_mask: u32,
    reference: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let recorded = record::cmd_set_stencil_reference(d, cb, face_mask, reference);
        d.latch(cb, recorded);
    });
}

// ==================================================================================================
// push constants
// ==================================================================================================

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
        let recorded = record::cmd_push_constants(d, cb, offset, bytes);
        d.latch(cb, recorded);
    });
}

/// `VkPushConstantsInfo` — the struct form `vkCmdPushConstants2` (core Vulkan 1.4, promoted from
/// `VK_KHR_maintenance6`) takes in place of the 1.0 positional arguments. Layout from `vk.xml`.
#[repr(C)]
pub struct VkPushConstantsInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub layout: u64,
    pub stage_flags: u32,
    pub offset: u32,
    pub size: u32,
    pub p_values: *const c_void,
}

/// `vkCmdPushConstants2` (core 1.4) is exactly `vkCmdPushConstants` with its arguments carried in a
/// struct — no new capability — so it forwards to the same `record::cmd_push_constants` lowering. It
/// was previously a silent no-op, which dropped the push constants and rendered wrong pixels with no
/// error. A NULL info is ignored, matching the 1.0 body's NULL-values behaviour.
pub extern "C" fn vkCmdPushConstants2(
    command_buffer: *mut c_void,
    p_push_constants_info: *const c_void,
) {
    let Some(info) = (unsafe { (p_push_constants_info as *const VkPushConstantsInfo).as_ref() })
    else {
        return;
    };
    vkCmdPushConstants(
        command_buffer,
        info.layout,
        info.stage_flags,
        info.offset,
        info.size,
        info.p_values,
    );
}

/// `vkCmdPushConstants2KHR` — the pre-promotion `VK_KHR_maintenance6` spelling of the same command.
pub extern "C" fn vkCmdPushConstants2KHR(
    command_buffer: *mut c_void,
    p_push_constants_info: *const c_void,
) {
    vkCmdPushConstants2(command_buffer, p_push_constants_info)
}
