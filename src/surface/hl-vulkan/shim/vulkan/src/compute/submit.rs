use super::*;

pub extern "C" fn vkQueueSubmit(
    _queue: *mut c_void,
    submit_count: u32,
    p_submits: *const c_void,
    fence: u64,
) -> VkResult {
    let submits = if p_submits.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(p_submits as *const VkSubmitInfo, submit_count as usize)
        }
    };
    // Gather every submitted command buffer (unwrapping each dispatchable to its u64 handle) plus every
    // queue-side timeline signal from each batch's VkTimelineSemaphoreSubmitInfo pNext.
    let mut cbs: Vec<VkCbHandle> = Vec::new();
    let mut timeline_signals: Vec<(u64, u64)> = Vec::new();
    for si in submits {
        if !si.p_command_buffers.is_null() {
            let ptrs = unsafe {
                std::slice::from_raw_parts(si.p_command_buffers, si.command_buffer_count as usize)
            };
            for &p in ptrs {
                if let Some(h) = unsafe { CommandBuffer::handle(p) } {
                    cbs.push(h);
                }
            }
        }
        // VkTimelineSemaphoreSubmitInfo::pSignalSemaphoreValues[i] pairs positionally with
        // VkSubmitInfo::pSignalSemaphores[i] — the value queue completion advances that semaphore to.
        let node = unsafe {
            ExtensionChain::find(si.p_next, VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO)
        };
        if !node.is_null() && !si.p_signal_semaphores.is_null() {
            let ts = unsafe { &*(node as *const VkTimelineSemaphoreSubmitInfo) };
            if !ts.p_signal_semaphore_values.is_null() {
                let n = (si.signal_semaphore_count as usize)
                    .min(ts.signal_semaphore_value_count as usize);
                let sems = unsafe { std::slice::from_raw_parts(si.p_signal_semaphores, n) };
                let vals = unsafe { std::slice::from_raw_parts(ts.p_signal_semaphore_values, n) };
                for i in 0..n {
                    timeline_signals.push((sems[i], vals[i]));
                }
            }
        }
    }
    let signal = if fence != 0 { Some(fence) } else { None };
    let r = ShimState::with_sink(|dev, sink| {
        let r = ResultStatus::from_gpu(submit_service::queue_submit(dev, sink, &cbs, signal));
        if r == VK_SUCCESS {
            dev.signal_timeline_values(&timeline_signals);
        }
        r
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED);
    // Error: a refused submit means the recorded work never ran. Latched by result code — submit runs
    // per frame and a persistent failure repeats every frame, but the set of `VkResult`s is small and
    // bounded, so each distinct reason still gets to say itself once.
    static REFUSED: crate::logging::Latch = crate::logging::Latch::new();
    if r != VK_SUCCESS && REFUSED.fires(r as u32 as u64) {
        hl_log::hl_error!(
            hl_log::tag::SHIM,
            "vkQueueSubmit cbs={} -> {:?}",
            cbs.len(),
            r
        );
    }
    r
}

pub extern "C" fn vkQueueWaitIdle(_queue: *mut c_void) -> VkResult {
    // The executor replays each submit synchronously, so the queue is idle on return.
    VK_SUCCESS
}

pub extern "C" fn vkDeviceWaitIdle(_device: *mut c_void) -> VkResult {
    VK_SUCCESS
}

// ---- synchronization2 submit + maintenance5 map/unmap-2 (delegate to the v1 bodies) ---------------

/// `vkQueueSubmit2` — the sync2 submit form. Gathers every `VkCommandBufferSubmitInfo::commandBuffer`
/// across the batch (unwrapping each dispatchable to its `u64` handle) and lowers exactly as
/// `vkQueueSubmit`; the semaphore-info arrays are validated-then-ignored by the synchronous model.
pub extern "C" fn vkQueueSubmit2(
    _queue: *mut c_void,
    submit_count: u32,
    p_submits: *const c_void,
    fence: u64,
) -> VkResult {
    let submits = if p_submits.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(p_submits as *const VkSubmitInfo2, submit_count as usize)
        }
    };
    let mut cbs: Vec<VkCbHandle> = Vec::new();
    let mut timeline_signals: Vec<(u64, u64)> = Vec::new();
    for si in submits {
        if !si.p_command_buffer_infos.is_null() {
            let infos = unsafe {
                std::slice::from_raw_parts(
                    si.p_command_buffer_infos,
                    si.command_buffer_info_count as usize,
                )
            };
            for info in infos {
                if let Some(h) = unsafe { CommandBuffer::handle(info.command_buffer) } {
                    cbs.push(h);
                }
            }
        }
        // sync2 carries the timeline value inline on each VkSemaphoreSubmitInfo (queue-side signal).
        if !si.p_signal_semaphore_infos.is_null() {
            let infos = unsafe {
                std::slice::from_raw_parts(
                    si.p_signal_semaphore_infos as *const VkSemaphoreSubmitInfo,
                    si.signal_semaphore_info_count as usize,
                )
            };
            for info in infos {
                timeline_signals.push((info.semaphore, info.value));
            }
        }
    }
    let signal = if fence != 0 { Some(fence) } else { None };
    ShimState::with_sink(|dev, sink| {
        let r = ResultStatus::from_gpu(submit_service::queue_submit(dev, sink, &cbs, signal));
        if r == VK_SUCCESS {
            dev.signal_timeline_values(&timeline_signals);
        }
        r
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

/// `vkQueueSubmit2KHR` — the `VK_KHR_synchronization2` alias.
pub extern "C" fn vkQueueSubmit2KHR(
    queue: *mut c_void,
    submit_count: u32,
    p_submits: *const c_void,
    fence: u64,
) -> VkResult {
    vkQueueSubmit2(queue, submit_count, p_submits, fence)
}

/// `vkQueueBindSparse` (core Vulkan 1.0) — a sparse-binding submission. `sparseBinding` and every other
/// sparse feature are reported `VK_FALSE`, so no sparse resource can have been created and the only batch
/// an application can legally submit is an empty one: that is a real submission with nothing to bind, and
/// it signals its fence exactly as an empty `vkQueueSubmit` does. A non-empty batch names memory ranges of
/// a resource that cannot exist, so it is `VK_ERROR_FEATURE_NOT_PRESENT` rather than a false success.
pub extern "C" fn vkQueueBindSparse(
    _queue: *mut c_void,
    bind_info_count: u32,
    p_bind_info: *const c_void,
    fence: u64,
) -> VkResult {
    if bind_info_count != 0 && !p_bind_info.is_null() {
        crate::stub::Call::unsupported("vkQueueBindSparse", "sparseBinding is not supported");
        return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    let signal = (fence != 0).then_some(fence);
    ShimState::with_sink(|dev, sink| {
        ResultStatus::from_gpu(submit_service::queue_submit(dev, sink, &[], signal))
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}
