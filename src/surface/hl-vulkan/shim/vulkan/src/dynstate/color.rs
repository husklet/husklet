use core::ffi::c_void;

use hl_vulkan::service::record;

use super::support::{CommandBuffer, DynamicState, ShimState};

pub extern "C" fn vkCmdSetLogicOpEXT(command_buffer: *mut c_void, logic_op: i32) {
    DynamicState::record(command_buffer, |ds| ds.logic_op = logic_op);
}

pub extern "C" fn vkCmdSetLogicOpEnableEXT(command_buffer: *mut c_void, logic_op_enable: u32) {
    DynamicState::record(command_buffer, |ds| {
        ds.logic_op_enable = logic_op_enable != 0
    });
}

pub extern "C" fn vkCmdSetColorBlendEnableEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    p_color_blend_enables: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_color_blend_enables.is_null() || attachment_count == 0 {
        return;
    }
    let vals = unsafe {
        core::slice::from_raw_parts(
            p_color_blend_enables as *const u32,
            attachment_count as usize,
        )
    }
    .to_vec();
    ShimState::with_device(|d| {
        let recorded = record::set_dynamic_attachment_array(d, cb, first_attachment, &vals, |ds| {
            &mut ds.color_blend_enables
        });
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetColorWriteMaskEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    p_color_write_masks: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_color_write_masks.is_null() || attachment_count == 0 {
        return;
    }
    let vals = unsafe {
        core::slice::from_raw_parts(p_color_write_masks as *const u32, attachment_count as usize)
    }
    .to_vec();
    ShimState::with_device(|d| {
        let recorded = record::set_dynamic_attachment_array(d, cb, first_attachment, &vals, |ds| {
            &mut ds.color_write_masks
        });
        d.latch(cb, recorded);
    });
}

pub extern "C" fn vkCmdSetColorWriteEnableEXT(
    command_buffer: *mut c_void,
    attachment_count: u32,
    p_color_write_enables: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_color_write_enables.is_null() || attachment_count == 0 {
        return;
    }
    let vals = unsafe {
        core::slice::from_raw_parts(
            p_color_write_enables as *const u32,
            attachment_count as usize,
        )
    }
    .to_vec();
    ShimState::with_device(|d| {
        // This one had a second, wholly silent refusal behind it: `set_dynamic_attachment_array`
        // rejects an attachment span beyond `maxColorAttachments` — a truthful usage error, added to
        // stop a hostile `first` resizing the state vector to gigabytes — and the caller was told
        // nothing about it at all.
        let recorded =
            record::set_dynamic_attachment_array(d, cb, 0, &vals, |ds| &mut ds.color_write_enables);
        d.latch(cb, recorded);
    });
}

/// `vkCmdSetColorBlendEquationEXT` — the per-attachment blend equation is unmodeled fixed-function state
/// (the color oracle does no blending). Record that the color-blend state was touched (mark the
/// attachments as blend-enabled slots so the state is observable) with no encoder op.
pub extern "C" fn vkCmdSetColorBlendEquationEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    _p_color_blend_equations: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if attachment_count == 0 {
        return;
    }
    // Ensure the blend-enable vector covers the touched attachments (honest observable state).
    let ext = vec![0u32; attachment_count as usize];
    ShimState::with_device(|d| {
        let recorded = record::set_dynamic_attachment_array(d, cb, first_attachment, &ext, |ds| {
            &mut ds.color_blend_enables
        });
        d.latch(cb, recorded);
    });
}

/// `vkCmdSetColorBlendAdvancedEXT` — advanced blend (VK_EXT_blend_operation_advanced) is not modeled;
/// record that the attachments were touched, no encoder op.
pub extern "C" fn vkCmdSetColorBlendAdvancedEXT(
    command_buffer: *mut c_void,
    first_attachment: u32,
    attachment_count: u32,
    _p_color_blend_advanced: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if attachment_count == 0 {
        return;
    }
    let ext = vec![0u32; attachment_count as usize];
    ShimState::with_device(|d| {
        let recorded = record::set_dynamic_attachment_array(d, cb, first_attachment, &ext, |ds| {
            &mut ds.color_blend_enables
        });
        d.latch(cb, recorded);
    });
}
