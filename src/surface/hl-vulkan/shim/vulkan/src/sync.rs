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

use hl_vulkan::result::vk_result_from_gpu_error;
use hl_vulkan::service::{record, sync};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::with;
use crate::types::*;

// VkResult / VkEventStatus values not already in scope via `types::*`.
const VK_EVENT_SET: VkResult = 3;
const VK_EVENT_RESET: VkResult = 4;

/// Run `f` with the logical device, or `VK_ERROR_INITIALIZATION_FAILED` if none has been created.
fn dev_res(f: impl FnOnce(&mut Device) -> VkResult) -> VkResult {
    with(|s| s.device.as_mut().map(f)).unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

unsafe fn cmdbuf_handle(p: *mut c_void) -> Option<VkCbHandle> {
    Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
}

/// Walk a `pNext` chain for a target `sType`.
unsafe fn find_pnext(mut p: *const c_void, target: i32) -> *const c_void {
    while !p.is_null() {
        let base = &*(p as *const VkBaseInStructure);
        if base.s_type == target {
            return p;
        }
        p = base.p_next as *const c_void;
    }
    core::ptr::null()
}

/// Parse a `VkSemaphoreCreateInfo` pNext for `VkSemaphoreTypeCreateInfo` → `(is_timeline, initial_value)`.
fn parse_semaphore_type(p_create_info: *const c_void) -> (bool, u64) {
    let Some(ci) = (unsafe { (p_create_info as *const VkSemaphoreCreateInfo).as_ref() }) else {
        return (false, 0);
    };
    let node = unsafe { find_pnext(ci.p_next, VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO) };
    if node.is_null() {
        return (false, 0);
    }
    let ti = unsafe { &*(node as *const VkSemaphoreTypeCreateInfo) };
    (ti.semaphore_type == VK_SEMAPHORE_TYPE_TIMELINE, ti.initial_value)
}

// ==================================================================================================
// semaphores (binary + timeline)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateSemaphore(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_semaphore: *mut u64,
) -> VkResult {
    let (timeline, initial) = parse_semaphore_type(p_create_info);
    let handle = with(|s| {
        let d = s.device.as_mut()?;
        Some(sync::create_semaphore(d, timeline, initial))
    });
    match handle {
        Some(h) => {
            if !p_semaphore.is_null() {
                unsafe { *p_semaphore = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroySemaphore(_device: *mut c_void, semaphore: u64, _p_allocator: *const c_void) {
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            sync::destroy_semaphore(d, semaphore);
        }
    });
}

/// `vkSignalSemaphore` — host signal of a timeline semaphore to `pSignalInfo->value`.
#[no_mangle]
pub extern "C" fn vkSignalSemaphore(_device: *mut c_void, p_signal_info: *const c_void) -> VkResult {
    let Some(info) = (unsafe { (p_signal_info as *const VkSemaphoreSignalInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    dev_res(|d| match sync::signal_semaphore(d, info.semaphore, info.value) {
        Ok(()) => VK_SUCCESS,
        Err(e) => vk_result_from_gpu_error(&e),
    })
}

/// `vkGetSemaphoreCounterValue` — read a timeline semaphore's counter.
#[no_mangle]
pub extern "C" fn vkGetSemaphoreCounterValue(
    _device: *mut c_void,
    semaphore: u64,
    p_value: *mut u64,
) -> VkResult {
    dev_res(|d| match sync::semaphore_counter(d, semaphore) {
        Ok(v) => {
            if let Some(out) = unsafe { p_value.as_mut() } {
                *out = v;
            }
            VK_SUCCESS
        }
        Err(e) => vk_result_from_gpu_error(&e),
    })
}

/// `vkWaitSemaphores` — wait for timeline semaphores to reach their values. Synchronous model: a
/// satisfied wait returns `VK_SUCCESS` immediately, an unsatisfiable one truthfully reports `VK_TIMEOUT`.
#[no_mangle]
pub extern "C" fn vkWaitSemaphores(_device: *mut c_void, p_wait_info: *const c_void, _timeout: u64) -> VkResult {
    let Some(info) = (unsafe { (p_wait_info as *const VkSemaphoreWaitInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.semaphore_count == 0 || info.p_semaphores.is_null() || info.p_values.is_null() {
        return VK_SUCCESS;
    }
    let sems = unsafe { std::slice::from_raw_parts(info.p_semaphores, info.semaphore_count as usize) };
    let vals = unsafe { std::slice::from_raw_parts(info.p_values, info.semaphore_count as usize) };
    let any = info.flags & VK_SEMAPHORE_WAIT_ANY_BIT != 0;
    dev_res(|d| if sync::wait_semaphores(d, sems, vals, any) { VK_SUCCESS } else { VK_TIMEOUT })
}

// ==================================================================================================
// events
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateEvent(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_event: *mut u64,
) -> VkResult {
    let handle = with(|s| s.device.as_mut().map(sync::create_event));
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

#[no_mangle]
pub extern "C" fn vkDestroyEvent(_device: *mut c_void, event: u64, _p_allocator: *const c_void) {
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            sync::destroy_event(d, event);
        }
    });
}

/// `vkGetEventStatus` — `VK_EVENT_SET` (3) if signaled, `VK_EVENT_RESET` (4) if not.
#[no_mangle]
pub extern "C" fn vkGetEventStatus(_device: *mut c_void, event: u64) -> VkResult {
    dev_res(|d| match sync::event_status(d, event) {
        Ok(true) => VK_EVENT_SET,
        Ok(false) => VK_EVENT_RESET,
        Err(e) => vk_result_from_gpu_error(&e),
    })
}

#[no_mangle]
pub extern "C" fn vkSetEvent(_device: *mut c_void, event: u64) -> VkResult {
    dev_res(|d| match sync::set_event(d, event, true) {
        Ok(()) => VK_SUCCESS,
        Err(e) => vk_result_from_gpu_error(&e),
    })
}

#[no_mangle]
pub extern "C" fn vkResetEvent(_device: *mut c_void, event: u64) -> VkResult {
    dev_res(|d| match sync::set_event(d, event, false) {
        Ok(()) => VK_SUCCESS,
        Err(e) => vk_result_from_gpu_error(&e),
    })
}

#[no_mangle]
pub extern "C" fn vkCmdSetEvent(command_buffer: *mut c_void, event: u64, _stage_mask: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_set_event(d, cb, event, true);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdResetEvent(command_buffer: *mut c_void, event: u64, _stage_mask: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_set_event(d, cb, event, false);
        }
    });
}

#[no_mangle]
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
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    let events = if event_count == 0 || p_events.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_events, event_count as usize) }.to_vec()
    };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_wait_events(d, cb, &events);
        }
    });
}

// ==================================================================================================
// query pools
// ==================================================================================================

#[no_mangle]
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
    dev_res(|d| match sync::create_query_pool(d, ci.query_type, ci.query_count) {
        Ok(h) => {
            if !p_query_pool.is_null() {
                unsafe { *p_query_pool = h };
            }
            VK_SUCCESS
        }
        Err(e) => vk_result_from_gpu_error(&e),
    })
}

#[no_mangle]
pub extern "C" fn vkDestroyQueryPool(_device: *mut c_void, query_pool: u64, _p_allocator: *const c_void) {
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            sync::destroy_query_pool(d, query_pool);
        }
    });
}

/// `vkGetQueryPoolResults` — copy the pool's results into the caller's buffer honouring the flag set.
#[no_mangle]
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
    dev_res(|d| {
        match sync::get_query_pool_results(
            d, query_pool, first_query, query_count, out, stride, wide, wait, with_avail, partial,
        ) {
            Ok(true) => VK_SUCCESS,
            Ok(false) => VK_NOT_READY,
            Err(e) => vk_result_from_gpu_error(&e),
        }
    })
}

/// `vkResetQueryPool` (Vulkan 1.2 / `VK_EXT_host_query_reset`) — host reset of a query-pool range.
#[no_mangle]
pub extern "C" fn vkResetQueryPool(_device: *mut c_void, query_pool: u64, first_query: u32, query_count: u32) {
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            sync::reset_query_pool(d, query_pool, first_query, query_count);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdBeginQuery(command_buffer: *mut c_void, query_pool: u64, query: u32, _flags: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_begin_query(d, cb, query_pool, query);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdEndQuery(command_buffer: *mut c_void, query_pool: u64, query: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_end_query(d, cb, query_pool, query);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdResetQueryPool(command_buffer: *mut c_void, query_pool: u64, first_query: u32, query_count: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_reset_query_pool(d, cb, query_pool, first_query, query_count);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdWriteTimestamp(
    command_buffer: *mut c_void,
    _pipeline_stage: u32,
    query_pool: u64,
    query: u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_write_timestamp(d, cb, query_pool, query);
        }
    });
}

// ==================================================================================================
// promoted-core / KHR / EXT aliases (delegate verbatim to the implemented base bodies)
// ==================================================================================================

/// `vkGetSemaphoreCounterValueKHR` — the `VK_KHR_timeline_semaphore` alias.
#[no_mangle]
pub extern "C" fn vkGetSemaphoreCounterValueKHR(device: *mut c_void, semaphore: u64, p_value: *mut u64) -> VkResult {
    vkGetSemaphoreCounterValue(device, semaphore, p_value)
}

/// `vkSignalSemaphoreKHR` — the `VK_KHR_timeline_semaphore` alias.
#[no_mangle]
pub extern "C" fn vkSignalSemaphoreKHR(device: *mut c_void, p_signal_info: *const c_void) -> VkResult {
    vkSignalSemaphore(device, p_signal_info)
}

/// `vkWaitSemaphoresKHR` — the `VK_KHR_timeline_semaphore` alias.
#[no_mangle]
pub extern "C" fn vkWaitSemaphoresKHR(device: *mut c_void, p_wait_info: *const c_void, timeout: u64) -> VkResult {
    vkWaitSemaphores(device, p_wait_info, timeout)
}

/// `vkResetQueryPoolEXT` — the `VK_EXT_host_query_reset` alias.
#[no_mangle]
pub extern "C" fn vkResetQueryPoolEXT(device: *mut c_void, query_pool: u64, first_query: u32, query_count: u32) {
    vkResetQueryPool(device, query_pool, first_query, query_count)
}

// ---- synchronization2 recording commands (core 1.3 / VK_KHR_synchronization2) ---------------------
// The sync2 forms carry 64-bit stage masks + a `VkDependencyInfo`; the modeled lowering ignores the
// stage/access masks (the IR is dependency-implicit) and reduces to the same device op as the v1 command.

/// `vkCmdWriteTimestamp2` — record a timestamp write (the 64-bit `stage` is not modeled). Same lowering
/// as `vkCmdWriteTimestamp`.
#[no_mangle]
pub extern "C" fn vkCmdWriteTimestamp2(command_buffer: *mut c_void, _stage: u64, query_pool: u64, query: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_write_timestamp(d, cb, query_pool, query);
        }
    });
}

/// `vkCmdWriteTimestamp2KHR` — the `VK_KHR_synchronization2` alias.
#[no_mangle]
pub extern "C" fn vkCmdWriteTimestamp2KHR(command_buffer: *mut c_void, stage: u64, query_pool: u64, query: u32) {
    vkCmdWriteTimestamp2(command_buffer, stage, query_pool, query)
}

/// `vkCmdSetEvent2` — record a device set of `event` (the `VkDependencyInfo` scope is not modeled).
#[no_mangle]
pub extern "C" fn vkCmdSetEvent2(command_buffer: *mut c_void, event: u64, _p_dependency_info: *const c_void) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_set_event(d, cb, event, true);
        }
    });
}

/// `vkCmdSetEvent2KHR` — the `VK_KHR_synchronization2` alias.
#[no_mangle]
pub extern "C" fn vkCmdSetEvent2KHR(command_buffer: *mut c_void, event: u64, p_dependency_info: *const c_void) {
    vkCmdSetEvent2(command_buffer, event, p_dependency_info)
}

/// `vkCmdResetEvent2` — record a device reset of `event` (the 64-bit `stageMask` is not modeled).
#[no_mangle]
pub extern "C" fn vkCmdResetEvent2(command_buffer: *mut c_void, event: u64, _stage_mask: u64) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_set_event(d, cb, event, false);
        }
    });
}

/// `vkCmdResetEvent2KHR` — the `VK_KHR_synchronization2` alias.
#[no_mangle]
pub extern "C" fn vkCmdResetEvent2KHR(command_buffer: *mut c_void, event: u64, stage_mask: u64) {
    vkCmdResetEvent2(command_buffer, event, stage_mask)
}

/// `vkCmdWaitEvents2` — validate the waited events (the per-event `VkDependencyInfo` array is not modeled).
/// Same lowering as `vkCmdWaitEvents`.
#[no_mangle]
pub extern "C" fn vkCmdWaitEvents2(
    command_buffer: *mut c_void,
    event_count: u32,
    p_events: *const u64,
    _p_dependency_infos: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    let events = if event_count == 0 || p_events.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_events, event_count as usize) }.to_vec()
    };
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_wait_events(d, cb, &events);
        }
    });
}

/// `vkCmdWaitEvents2KHR` — the `VK_KHR_synchronization2` alias.
#[no_mangle]
pub extern "C" fn vkCmdWaitEvents2KHR(
    command_buffer: *mut c_void,
    event_count: u32,
    p_events: *const u64,
    p_dependency_infos: *const c_void,
) {
    vkCmdWaitEvents2(command_buffer, event_count, p_events, p_dependency_infos)
}

#[no_mangle]
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
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else { return };
    let wide = flags & VK_QUERY_RESULT_64_BIT != 0;
    let with_avail = flags & VK_QUERY_RESULT_WITH_AVAILABILITY_BIT != 0;
    with(|s| {
        if let Some(d) = s.device.as_mut() {
            let _ = record::cmd_copy_query_pool_results(
                d, cb, query_pool, first_query, query_count, dst_buffer, dst_offset, stride, wide, with_avail,
            );
        }
    });
}
