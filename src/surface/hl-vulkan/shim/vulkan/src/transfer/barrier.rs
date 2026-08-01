//! Pipeline barrier commands.

use core::ffi::c_void;

use hl_vulkan::service::record;

use super::{CommandBuffer, ShimState};
use crate::types::*;

#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdPipelineBarrier(
    command_buffer: *mut c_void,
    _src_stage_mask: u32,
    _dst_stage_mask: u32,
    _dependency_flags: u32,
    _memory_barrier_count: u32,
    _p_memory_barriers: *const c_void,
    _buffer_memory_barrier_count: u32,
    _p_buffer_memory_barriers: *const c_void,
    image_memory_barrier_count: u32,
    p_image_memory_barriers: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let barriers = if image_memory_barrier_count == 0 || p_image_memory_barriers.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                p_image_memory_barriers as *const VkImageMemoryBarrier,
                image_memory_barrier_count as usize,
            )
        }
    };
    let transitions: Vec<(u64, i32, i32)> = barriers
        .iter()
        .map(|barrier| (barrier.image, barrier.old_layout, barrier.new_layout))
        .collect();
    ShimState::with_device(|device| {
        let recorded = record::cmd_pipeline_barrier(device, command_buffer, &transitions);
        device.latch(command_buffer, recorded);
    });
}

pub extern "C" fn vkCmdPipelineBarrier2(
    command_buffer: *mut c_void,
    p_dependency_info: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(dependency) = (unsafe { (p_dependency_info as *const VkDependencyInfo).as_ref() })
    else {
        return;
    };
    let barriers = if dependency.image_memory_barrier_count == 0
        || dependency.p_image_memory_barriers.is_null()
    {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                dependency.p_image_memory_barriers,
                dependency.image_memory_barrier_count as usize,
            )
        }
    };
    let transitions: Vec<(u64, i32, i32)> = barriers
        .iter()
        .map(|barrier| (barrier.image, barrier.old_layout, barrier.new_layout))
        .collect();
    ShimState::with_device(|device| {
        let recorded = record::cmd_pipeline_barrier(device, command_buffer, &transitions);
        device.latch(command_buffer, recorded);
    });
}

pub extern "C" fn vkCmdPipelineBarrier2KHR(
    command_buffer: *mut c_void,
    p_dependency_info: *const c_void,
) {
    vkCmdPipelineBarrier2(command_buffer, p_dependency_info);
}
