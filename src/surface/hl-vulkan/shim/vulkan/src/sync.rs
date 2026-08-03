//! Synchronization + query object entry points: `VkEvent`, `VkSemaphore` (binary + timeline), and
//! `VkQueryPool` — the ops real apps record/poll every frame.
//!
//! Host object lifecycle + host set/reset/poll (`vkCreate*`/`vkDestroy*`/`vkSetEvent`/`vkSignalSemaphore`/
//! `vkGetQueryPoolResults`/…) marshal the C ABI and call the `hl_vulkan` [`sync`](hl_vulkan::service::sync)
//! services; the device-side recording commands (`vkCmdSetEvent`/`vkCmdBeginQuery`/`vkCmdWriteTimestamp`/…)
//! call [`record`](hl_vulkan::service::record) and resolve at (synchronous) submit completion. Ported from
//! `hl-shim-vk/src/{event.rs,ext.rs,query.rs}`. Truthful failure: a wait that cannot be satisfied reports
//! `VK_TIMEOUT`, an unknown/misused handle its accurate error — never a false `VK_SUCCESS`.

use core::ffi::c_void;

use hl_vulkan::result::Status;
use hl_vulkan::service::{record, sync};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::StateStore;
use crate::types::*;

#[path = "sync_semaphore.rs"]
mod semaphore;

pub use semaphore::*;

// VkResult / VkEventStatus values not already in scope via `types::*`.
const VK_EVENT_SET: VkResult = 3;
const VK_EVENT_RESET: VkResult = 4;

/// Run `f` with the logical device, or `VK_ERROR_INITIALIZATION_FAILED` if none has been created.
struct ShimState;
impl ShimState {
    fn with_device_result(f: impl FnOnce(&mut Device) -> VkResult) -> VkResult {
        StateStore::with(|s| s.device_mut().map(f)).unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
    }

    fn with_device_sink_result(
        f: impl FnOnce(&mut Device, &mut dyn hl_gpu::CommandSink) -> VkResult,
    ) -> VkResult {
        StateStore::with(|s| s.device_and_sink().map(|(device, sink)| f(device, sink)))
            .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
    }
}

struct CommandBuffer;
impl CommandBuffer {
    unsafe fn handle(p: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
    }
}

// ==================================================================================================
// events
// ==================================================================================================

pub extern "C" fn vkCreateEvent(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_event: *mut u64,
) -> VkResult {
    let handle = StateStore::with(|s| s.device_mut().map(Device::create_event));
    match handle {
        Some(h) => {
            if !p_event.is_null() {
                unsafe { *p_event = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

pub extern "C" fn vkDestroyEvent(_device: *mut c_void, event: u64, _p_allocator: *const c_void) {
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            d.destroy_event(event);
        }
    });
}

/// `vkGetEventStatus` — `VK_EVENT_SET` (3) if signaled, `VK_EVENT_RESET` (4) if not.
pub extern "C" fn vkGetEventStatus(_device: *mut c_void, event: u64) -> VkResult {
    ShimState::with_device_result(|d| match d.event_status(event) {
        Ok(true) => VK_EVENT_SET,
        Ok(false) => VK_EVENT_RESET,
        Err(e) => Status::from_error(&e),
    })
}

pub extern "C" fn vkSetEvent(_device: *mut c_void, event: u64) -> VkResult {
    ShimState::with_device_result(|d| match d.set_event(event, true) {
        Ok(()) => VK_SUCCESS,
        Err(e) => Status::from_error(&e),
    })
}

pub extern "C" fn vkResetEvent(_device: *mut c_void, event: u64) -> VkResult {
    ShimState::with_device_result(|d| match d.set_event(event, false) {
        Ok(()) => VK_SUCCESS,
        Err(e) => Status::from_error(&e),
    })
}

pub extern "C" fn vkCmdSetEvent(command_buffer: *mut c_void, event: u64, _stage_mask: u32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_set_event(d, cb, event, true);
            d.latch(cb, recorded);
        }
    });
}

pub extern "C" fn vkCmdResetEvent(command_buffer: *mut c_void, event: u64, _stage_mask: u32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_set_event(d, cb, event, false);
            d.latch(cb, recorded);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdWaitEvents(
    command_buffer: *mut c_void,
    event_count: u32,
    p_events: *const u64,
    _src_stage_mask: u32,
    _dst_stage_mask: u32,
    _memory_barrier_count: u32,
    _p_memory_barriers: *const c_void,
    _buffer_memory_barrier_count: u32,
    _p_buffer_memory_barriers: *const c_void,
    _image_memory_barrier_count: u32,
    _p_image_memory_barriers: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let events = if event_count == 0 || p_events.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_events, event_count as usize) }.to_vec()
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_wait_events(d, cb, &events);
            d.latch(cb, recorded);
        }
    });
}

// ==================================================================================================
// query pools
// ==================================================================================================

pub extern "C" fn vkCreateQueryPool(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_query_pool: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkQueryPoolCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_query_pool.is_null() {
        unsafe { *p_query_pool = 0 };
    }
    ShimState::with_device_result(|d| {
        match sync::create_query_pool(d, ci.query_type, ci.query_count) {
            Ok(h) => {
                if !p_query_pool.is_null() {
                    unsafe { *p_query_pool = h };
                }
                VK_SUCCESS
            }
            Err(e) => Status::from_error(&e),
        }
    })
}

pub extern "C" fn vkDestroyQueryPool(
    _device: *mut c_void,
    query_pool: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            d.destroy_query_pool(query_pool);
        }
    });
}

/// `vkGetQueryPoolResults` — copy the pool's results into the caller's buffer honouring the flag set.
#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkGetQueryPoolResults(
    _device: *mut c_void,
    query_pool: u64,
    first_query: u32,
    query_count: u32,
    data_size: usize,
    p_data: *mut c_void,
    stride: u64,
    flags: u32,
) -> VkResult {
    if p_data.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(p_data as *mut u8, data_size) };
    let wide = flags & VK_QUERY_RESULT_64_BIT != 0;
    let wait = flags & VK_QUERY_RESULT_WAIT_BIT != 0;
    let with_avail = flags & VK_QUERY_RESULT_WITH_AVAILABILITY_BIT != 0;
    let partial = flags & VK_QUERY_RESULT_PARTIAL_BIT != 0;
    ShimState::with_device_result(|d| {
        match sync::get_query_pool_results(
            d,
            query_pool,
            first_query,
            query_count,
            out,
            stride,
            wide,
            wait,
            with_avail,
            partial,
        ) {
            Ok(true) => VK_SUCCESS,
            Ok(false) => VK_NOT_READY,
            Err(e) => Status::from_error(&e),
        }
    })
}

/// `vkResetQueryPool` (Vulkan 1.2 / `VK_EXT_host_query_reset`) — host reset of a query-pool range.
pub extern "C" fn vkResetQueryPool(
    _device: *mut c_void,
    query_pool: u64,
    first_query: u32,
    query_count: u32,
) {
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            sync::reset_query_pool(d, query_pool, first_query, query_count);
        }
    });
}

pub extern "C" fn vkCmdBeginQuery(
    command_buffer: *mut c_void,
    query_pool: u64,
    query: u32,
    _flags: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_begin_query(d, cb, query_pool, query);
            d.latch(cb, recorded);
        }
    });
}

pub extern "C" fn vkCmdEndQuery(command_buffer: *mut c_void, query_pool: u64, query: u32) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_end_query(d, cb, query_pool, query);
            d.latch(cb, recorded);
        }
    });
}

pub extern "C" fn vkCmdResetQueryPool(
    command_buffer: *mut c_void,
    query_pool: u64,
    first_query: u32,
    query_count: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded =
                record::cmd_reset_query_pool(d, cb, query_pool, first_query, query_count);
            d.latch(cb, recorded);
        }
    });
}

pub extern "C" fn vkCmdWriteTimestamp(
    command_buffer: *mut c_void,
    _pipeline_stage: u32,
    query_pool: u64,
    query: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_write_timestamp(d, cb, query_pool, query);
            d.latch(cb, recorded);
        }
    });
}

// ==================================================================================================
// promoted-core / KHR / EXT aliases (delegate verbatim to the implemented base bodies)
// ==================================================================================================

/// `vkResetQueryPoolEXT` — the `VK_EXT_host_query_reset` alias.
pub extern "C" fn vkResetQueryPoolEXT(
    device: *mut c_void,
    query_pool: u64,
    first_query: u32,
    query_count: u32,
) {
    vkResetQueryPool(device, query_pool, first_query, query_count)
}

// ---- synchronization2 recording commands (core 1.3 / VK_KHR_synchronization2) ---------------------
// The sync2 forms carry 64-bit stage masks + a `VkDependencyInfo`; the modeled lowering ignores the
// stage/access masks (the IR is dependency-implicit) and reduces to the same device op as the v1 command.

/// `vkCmdWriteTimestamp2` — record a timestamp write (the 64-bit `stage` is not modeled). Same lowering
/// as `vkCmdWriteTimestamp`.
pub extern "C" fn vkCmdWriteTimestamp2(
    command_buffer: *mut c_void,
    _stage: u64,
    query_pool: u64,
    query: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_write_timestamp(d, cb, query_pool, query);
            d.latch(cb, recorded);
        }
    });
}

/// `vkCmdWriteTimestamp2KHR` — the `VK_KHR_synchronization2` alias.
pub extern "C" fn vkCmdWriteTimestamp2KHR(
    command_buffer: *mut c_void,
    stage: u64,
    query_pool: u64,
    query: u32,
) {
    vkCmdWriteTimestamp2(command_buffer, stage, query_pool, query)
}

/// `vkCmdSetEvent2` — record a device set of `event` (the `VkDependencyInfo` scope is not modeled).
pub extern "C" fn vkCmdSetEvent2(
    command_buffer: *mut c_void,
    event: u64,
    _p_dependency_info: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_set_event(d, cb, event, true);
            d.latch(cb, recorded);
        }
    });
}

/// `vkCmdSetEvent2KHR` — the `VK_KHR_synchronization2` alias.
pub extern "C" fn vkCmdSetEvent2KHR(
    command_buffer: *mut c_void,
    event: u64,
    p_dependency_info: *const c_void,
) {
    vkCmdSetEvent2(command_buffer, event, p_dependency_info)
}

/// `vkCmdResetEvent2` — record a device reset of `event` (the 64-bit `stageMask` is not modeled).
pub extern "C" fn vkCmdResetEvent2(command_buffer: *mut c_void, event: u64, _stage_mask: u64) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_set_event(d, cb, event, false);
            d.latch(cb, recorded);
        }
    });
}

/// `vkCmdResetEvent2KHR` — the `VK_KHR_synchronization2` alias.
pub extern "C" fn vkCmdResetEvent2KHR(command_buffer: *mut c_void, event: u64, stage_mask: u64) {
    vkCmdResetEvent2(command_buffer, event, stage_mask)
}

/// `vkCmdWaitEvents2` — validate the waited events (the per-event `VkDependencyInfo` array is not modeled).
/// Same lowering as `vkCmdWaitEvents`.
pub extern "C" fn vkCmdWaitEvents2(
    command_buffer: *mut c_void,
    event_count: u32,
    p_events: *const u64,
    _p_dependency_infos: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let events = if event_count == 0 || p_events.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_events, event_count as usize) }.to_vec()
    };
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_wait_events(d, cb, &events);
            d.latch(cb, recorded);
        }
    });
}

/// `vkCmdWaitEvents2KHR` — the `VK_KHR_synchronization2` alias.
pub extern "C" fn vkCmdWaitEvents2KHR(
    command_buffer: *mut c_void,
    event_count: u32,
    p_events: *const u64,
    p_dependency_infos: *const c_void,
) {
    vkCmdWaitEvents2(command_buffer, event_count, p_events, p_dependency_infos)
}

#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdCopyQueryPoolResults(
    command_buffer: *mut c_void,
    query_pool: u64,
    first_query: u32,
    query_count: u32,
    dst_buffer: u64,
    dst_offset: u64,
    stride: u64,
    flags: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let wide = flags & VK_QUERY_RESULT_64_BIT != 0;
    let with_avail = flags & VK_QUERY_RESULT_WITH_AVAILABILITY_BIT != 0;
    StateStore::with(|s| {
        if let Some(d) = s.device_mut() {
            let recorded = record::cmd_copy_query_pool_results(
                d,
                cb,
                query_pool,
                first_query,
                query_count,
                dst_buffer,
                dst_offset,
                stride,
                wide,
                with_avail,
            );
            d.latch(cb, recorded);
        }
    });
}
