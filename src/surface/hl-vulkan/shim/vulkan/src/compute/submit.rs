use super::*;
use hl_gpu::{CommandSink, GpuError, TimelineWait};

struct QueueSubmission {
    command_buffers: Vec<VkCbHandle>,
    waits: Vec<(u64, u64)>,
    signals: Vec<(u64, u64)>,
}

type ResolvedExternalWait = (usize, u64, hl_gpu::SyncExportId, u64, u64);

fn resolve_external_waits(
    submissions: &[QueueSubmission],
) -> std::result::Result<(usize, Vec<ResolvedExternalWait>), VkResult> {
    let device_token = StateStore::with(|state| state.current_device_token());
    let mut resolved = Vec::new();
    for submission in submissions {
        for &(semaphore, value) in &submission.waits {
            loop {
                let snapshot = StateStore::with(|state| {
                    if state.current_device_token() != device_token {
                        return None;
                    }
                    let record = state.device_ref()?.semaphores.get(&semaphore)?;
                    let export = record.shared?;
                    Some((export, record.generation, record.counter < value, state.sink.observer()))
                });
                let Some((export, generation, needs_wait, mut observer)) = snapshot else {
                    break;
                };
                if !needs_wait {
                    resolved.push((device_token, semaphore, export, generation, value));
                    break;
                }
                if let Err(error) = observer.import_sync(export) {
                    return Err(Status::from_error(&error));
                }
                let wait_result = observer.wait_sync(export, value, u64::MAX);
                let release_result = observer.release_sync(export);
                if let Err(error) = release_result {
                    return Err(Status::from_error(&error));
                }
                match wait_result {
                    Ok(TimelineWait::Reached) => {}
                    Ok(TimelineWait::Timeout) => return Err(VK_TIMEOUT),
                    Err(error) => return Err(Status::from_error(&error)),
                }
                resolved.push((device_token, semaphore, export, generation, value));
                break;
            }
        }
    }
    Ok((device_token, resolved))
}

fn commit_resolved_waits(dev: &mut hl_vulkan::Device, device_token: usize, waits: &[ResolvedExternalWait]) -> bool {
    for &(owner, semaphore, export, generation, value) in waits {
        if owner != device_token { return false; }
        let Some(record) = dev.semaphores.get_mut(&semaphore) else { return false; };
        if record.shared != Some(export) || record.generation != generation { return false; }
        record.counter = record.counter.max(value);
    }
    true
}

fn execute_submissions(
    dev: &mut hl_vulkan::Device,
    sink: &mut dyn hl_gpu::CommandSink,
    submissions: &[QueueSubmission],
    fence: Option<u64>,
) -> VkResult {
    if submissions.is_empty() {
        return ResultStatus::from_gpu(submit_service::queue_submit(dev, sink, &[], fence));
    }
    for (index, submission) in submissions.iter().enumerate() {
        let semaphore_snapshot = dev.semaphores.clone();
        if let Err(error) = dev.consume_queue_waits(sink, &submission.waits) {
            return Status::from_error(&error);
        }
        if let Err(error) = dev.validate_queue_signals(&submission.signals) {
            dev.semaphores = semaphore_snapshot;
            return Status::from_error(&error);
        }
        let signal_fence = (index + 1 == submissions.len()).then_some(fence).flatten();
        let submit_result = submit_service::queue_submit_outcome(
            dev,
            sink,
            &submission.command_buffers,
            signal_fence,
        );
        if let Err(outcome) = submit_result {
            let error = outcome.error();
            let definitely_not_committed = !outcome.committed() && match error {
                GpuError::Transport(transport) => {
                    transport.refusal() || transport.retryable_before_request()
                }
                _ => true,
            };
            if definitely_not_committed {
                dev.semaphores = semaphore_snapshot;
            }
            return Status::from_error(error);
        }
        if let Err(error) = dev.signal_queue_semaphores(sink, &submission.signals) {
            return Status::from_error(&error);
        }
    }
    VK_SUCCESS
}

fn execute_in_queue_order(submissions: &[QueueSubmission], fence: Option<u64>) -> VkResult {
    if submissions.is_empty() {
        return ShimState::with_sink(|device, sink| {
            execute_submissions(device, sink, submissions, fence)
        })
        .unwrap_or(VK_ERROR_INITIALIZATION_FAILED);
    }
    for (index, submission) in submissions.iter().enumerate() {
        let (device_token, resolved_waits) = match resolve_external_waits(std::slice::from_ref(submission)) {
            Ok(resolved) => resolved,
            Err(status) => return status,
        };
        let signal_fence = (index + 1 == submissions.len()).then_some(fence).flatten();
        let result = StateStore::with(|state| {
            if state.current_device_token() != device_token {
                return VK_ERROR_DEVICE_LOST;
            }
            let Some((device, sink)) = state.device_and_sink() else {
                return VK_ERROR_DEVICE_LOST;
            };
            if !commit_resolved_waits(device, device_token, &resolved_waits) {
                return VK_ERROR_DEVICE_LOST;
            }
            execute_submissions(device, sink, std::slice::from_ref(submission), signal_fence)
        });
        if result != VK_SUCCESS {
            return result;
        }
    }
    VK_SUCCESS
}

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
    let mut lowered = Vec::with_capacity(submits.len());
    for si in submits {
        let mut cbs = Vec::new();
        let mut waits = Vec::new();
        let mut signals = Vec::new();
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
        let timeline = (!node.is_null()).then(|| unsafe {
            &*(node as *const VkTimelineSemaphoreSubmitInfo)
        });
        if !si.p_wait_semaphores.is_null() {
            let sems = unsafe {
                std::slice::from_raw_parts(si.p_wait_semaphores, si.wait_semaphore_count as usize)
            };
            for (index, &semaphore) in sems.iter().enumerate() {
                let value = timeline
                    .filter(|info| !info.p_wait_semaphore_values.is_null())
                    .filter(|info| index < info.wait_semaphore_value_count as usize)
                    .map(|info| unsafe { *info.p_wait_semaphore_values.add(index) })
                    .unwrap_or(0);
                waits.push((semaphore, value));
            }
        }
        if !si.p_signal_semaphores.is_null() {
            let sems = unsafe {
                std::slice::from_raw_parts(si.p_signal_semaphores, si.signal_semaphore_count as usize)
            };
            for (index, &semaphore) in sems.iter().enumerate() {
                let value = timeline
                    .filter(|info| !info.p_signal_semaphore_values.is_null())
                    .filter(|info| index < info.signal_semaphore_value_count as usize)
                    .map(|info| unsafe { *info.p_signal_semaphore_values.add(index) })
                    .unwrap_or(0);
                signals.push((semaphore, value));
            }
        }
        lowered.push(QueueSubmission {
            command_buffers: cbs,
            waits,
            signals,
        });
    }
    let signal = if fence != 0 { Some(fence) } else { None };
    let cb_count: usize = lowered.iter().map(|submission| submission.command_buffers.len()).sum();
    let r = execute_in_queue_order(&lowered, signal);
    // Error: a refused submit means the recorded work never ran. Latched by result code — submit runs
    // per frame and a persistent failure repeats every frame, but the set of `VkResult`s is small and
    // bounded, so each distinct reason still gets to say itself once.
    static REFUSED: crate::logging::Latch = crate::logging::Latch::new();
    if r != VK_SUCCESS && REFUSED.fires(r as u32 as u64) {
        hl_log::hl_error!(
            hl_log::tag::SHIM,
            "vkQueueSubmit cbs={} -> {:?}",
            cb_count,
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
    let mut lowered = Vec::with_capacity(submits.len());
    for si in submits {
        let mut cbs = Vec::new();
        let mut waits = Vec::new();
        let mut signals = Vec::new();
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
        if !si.p_wait_semaphore_infos.is_null() {
            let infos = unsafe {
                std::slice::from_raw_parts(
                    si.p_wait_semaphore_infos as *const VkSemaphoreSubmitInfo,
                    si.wait_semaphore_info_count as usize,
                )
            };
            waits.extend(infos.iter().map(|info| (info.semaphore, info.value)));
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
                signals.push((info.semaphore, info.value));
            }
        }
        lowered.push(QueueSubmission {
            command_buffers: cbs,
            waits,
            signals,
        });
    }
    let signal = if fence != 0 { Some(fence) } else { None };
    execute_in_queue_order(&lowered, signal)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_submissions_allow_signal_then_wait() {
        let instance = hl_vulkan::Instance::new(hl_vulkan::result::HL_API_VERSION);
        let mut device = instance.create_device();
        let semaphore = hl_vulkan::service::sync::create_semaphore(&mut device, false, 0);
        let mut sink = hl_gpu::RecordingSink::with_full_caps();
        let submissions = [
            QueueSubmission {
                command_buffers: Vec::new(),
                waits: Vec::new(),
                signals: vec![(semaphore, 0)],
            },
            QueueSubmission {
                command_buffers: Vec::new(),
                waits: vec![(semaphore, 0)],
                signals: Vec::new(),
            },
        ];

        assert_eq!(
            execute_submissions(&mut device, &mut sink, &submissions, None),
            VK_SUCCESS
        );
        assert!(!device.semaphores[&semaphore].signaled);
        assert_eq!(sink.batches.len(), 2);
    }

    #[test]
    fn reimport_between_unlocked_wait_and_queue_lock_rejects_stale_resolution() {
        let instance = hl_vulkan::Instance::new(hl_vulkan::result::HL_API_VERSION);
        let mut device = instance.create_device();
        let semaphore = hl_vulkan::service::sync::create_semaphore(&mut device, true, 0);
        let first = hl_gpu::SyncExportId::from_parts(1, 11);
        let second = hl_gpu::SyncExportId::from_parts(2, 22);
        let generation = {
            let record = device.semaphores.get_mut(&semaphore).unwrap();
            record.shared = Some(first);
            record.generation = 4;
            record.generation
        };
        let resolved = [(7, semaphore, first, generation, 9)];
        // Models a concurrent permanent import after the observer wait returned.
        let record = device.semaphores.get_mut(&semaphore).unwrap();
        record.shared = Some(second);
        record.generation += 1;
        record.counter = 0;
        assert!(!commit_resolved_waits(&mut device, 7, &resolved));
        assert_eq!(device.semaphores[&semaphore].counter, 0);
    }
}
