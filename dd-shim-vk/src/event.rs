//! `VkEvent` — host + device set/reset/poll (real bodies).
//!
//! Ported from MoltenVK `GPUObjects/MVKSync.mm` (`MVKEvent`): an event is a guest-side boolean created
//! unsignaled. Host ops (`vkSetEvent`/`vkResetEvent`/`vkGetEventStatus`) mutate/poll it directly;
//! device ops (`vkCmdSetEvent`/`vkCmdResetEvent`) take effect when the submission completes — which is
//! synchronous in our host replay, so a device-set event is observably signaled once `vkQueueSubmit`
//! returns. `vkCmdWaitEvents` shares `vkCmdPipelineBarrier`'s image-transition recording (the waited
//! dependency resolves at submit completion in this single-queue model).

use crate::reg::{self, DeferredOp, EventRec};
use crate::types::*;
use ash::vk;
use core::ffi::c_void;

#[no_mangle]
pub extern "C" fn vkCreateEvent(
    _device: VkDevice,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_event: *mut VkEvent,
) -> VkResult {
    let Some(out) = (unsafe { p_event.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.events.insert(handle, EventRec { signaled: false });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyEvent(_device: VkDevice, event: VkEvent, _p_allocator: *const c_void) {
    reg::lock().events.remove(&event);
}

/// `vkGetEventStatus` — `VK_EVENT_SET` (3) if signaled, `VK_EVENT_RESET` (4) if not (spec §6.3).
#[no_mangle]
pub extern "C" fn vkGetEventStatus(_device: VkDevice, event: VkEvent) -> VkResult {
    // VK_EVENT_SET = 3, VK_EVENT_RESET = 4.
    match reg::lock().events.get(&event) {
        Some(e) if e.signaled => 3,
        Some(_) => 4,
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkSetEvent(_device: VkDevice, event: VkEvent) -> VkResult {
    match reg::lock().events.get_mut(&event) {
        Some(e) => {
            e.signaled = true;
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkResetEvent(_device: VkDevice, event: VkEvent) -> VkResult {
    match reg::lock().events.get_mut(&event) {
        Some(e) => {
            e.signaled = false;
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// Record a device set/reset of an event, applied at submit completion. A missing event or a
/// non-recording buffer records nothing.
fn cmd_event(command_buffer: VkCommandBuffer, event: VkEvent, set: bool) {
    let mut s = reg::lock();
    if !s.events.contains_key(&event) {
        return;
    }
    if let Some(rec) = s.recording_mut(command_buffer as usize) {
        rec.deferred.push(DeferredOp::Event { event, set });
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetEvent(
    command_buffer: VkCommandBuffer,
    event: VkEvent,
    _stage_mask: vk::PipelineStageFlags,
) {
    cmd_event(command_buffer, event, true);
}

#[no_mangle]
pub extern "C" fn vkCmdResetEvent(
    command_buffer: VkCommandBuffer,
    event: VkEvent,
    _stage_mask: vk::PipelineStageFlags,
) {
    cmd_event(command_buffer, event, false);
}

/// `vkCmdWaitEvents` — wait on events then apply the accompanying barriers. The waited events must all
/// exist; their image-memory barriers are recorded through the shared `vkCmdPipelineBarrier` path (they
/// participate in the same atomic submit-time layout validation).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdWaitEvents(
    command_buffer: VkCommandBuffer,
    event_count: u32,
    p_events: *const VkEvent,
    src_stage_mask: vk::PipelineStageFlags,
    dst_stage_mask: vk::PipelineStageFlags,
    _memory_barrier_count: u32,
    _p_memory_barriers: *const vk::MemoryBarrier,
    _buffer_memory_barrier_count: u32,
    _p_buffer_memory_barriers: *const vk::BufferMemoryBarrier,
    image_memory_barrier_count: u32,
    p_image_memory_barriers: *const vk::ImageMemoryBarrier,
) {
    if event_count != 0 && p_events.is_null() {
        return;
    }
    // Every waited event must exist (a wait on an unknown event is a usage error → record nothing).
    {
        let s = reg::lock();
        let events = if event_count == 0 {
            &[][..]
        } else {
            unsafe { core::slice::from_raw_parts(p_events, event_count as usize) }
        };
        if !events.iter().all(|e| s.events.contains_key(e)) {
            return;
        }
    }
    crate::command::record_image_barriers(
        command_buffer,
        src_stage_mask,
        dst_stage_mask,
        image_memory_barrier_count,
        p_image_memory_barriers,
    );
}
