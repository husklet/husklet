use super::*;

// ==================================================================================================
// indirect draws (validated; the IR carries no indirect draw op — a documented bring-up limit)
// ==================================================================================================

pub extern "C" fn vkCmdDrawIndirect(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    draw_count: u32,
    stride: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_draw_indirect(d, cb, buffer, offset, draw_count, stride);
    });
}

pub extern "C" fn vkCmdDrawIndexedIndirect(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    draw_count: u32,
    stride: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let _ = record::cmd_draw_indexed_indirect(d, cb, buffer, offset, draw_count, stride);
    });
}
