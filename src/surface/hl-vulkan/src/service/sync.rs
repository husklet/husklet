//! Host-side synchronization + query object lifecycle — `VkEvent`, timeline `VkSemaphore`, `VkQueryPool`.
//!
//! Ported from `hl-shim-vk/src/{event.rs,ext.rs,query.rs}` (themselves mirroring MoltenVK's `MVKEvent`,
//! `MVKTimelineSemaphore`, `MVKQueryPool`). These are the HOST ops — no IR is emitted; they mutate/poll
//! the [`Device`] model directly. The DEVICE ops (`vkCmd*`) that record into a command buffer and resolve
//! at submit completion live in [`super::record`] / [`super::submit`].
//!
//! Bounded-domain truthfulness: the host replay is synchronous, so an unmet timeline wait honestly
//! reports a timeout (it never blocks on a counter that cannot advance here), and occlusion /
//! pipeline-statistics query values are a conservative `0` (no real GPU sample count). Availability, the
//! timeline counter, and the read machinery are real and observable.

use crate::model::sync::{EventRec, QueryPoolRec, QueryResult, SemaphoreRec};
use crate::*;
use hl_gpu::{CommandSink, GpuError, Result, SyncExportId, TimelineWait};

// ---- events --------------------------------------------------------------------------------------

/// `vkCreateEvent` — mint an unsignaled event. No IR.
impl Device {
    pub fn create_event(&mut self) -> VkEvent {
        let handle = self.alloc_handle();
        self.events.insert(handle, EventRec { signaled: false });
        handle
    }

    /// `vkDestroyEvent` — drop the event (no-op on unknown / `VK_NULL_HANDLE`).
    pub fn destroy_event(&mut self, event: VkEvent) {
        self.events.remove(&event);
    }

    /// `vkSetEvent` (`set = true`) / `vkResetEvent` (`set = false`) — host set/clear. Errors on unknown.
    pub fn set_event(&mut self, event: VkEvent, set: bool) -> Result<()> {
        self.events
            .get_mut(&event)
            .ok_or(GpuError::Invalid("vkSetEvent/ResetEvent: unknown VkEvent"))?
            .signaled = set;
        Ok(())
    }

    /// `vkGetEventStatus` — the event's current signaled state (`true` → `VK_EVENT_SET`). Errors on unknown.
    pub fn event_status(&self, event: VkEvent) -> Result<bool> {
        Ok(self
            .events
            .get(&event)
            .ok_or(GpuError::Invalid("vkGetEventStatus: unknown VkEvent"))?
            .signaled)
    }
}

pub fn export_semaphore(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphore: VkSemaphore,
) -> Result<SyncExportId> {
    let rec = dev
        .semaphores
        .get_mut(&semaphore)
        .ok_or(GpuError::Invalid("unknown VkSemaphore"))?;
    if !rec.timeline {
        return Err(GpuError::Invalid("external semaphore must be timeline"));
    }
    if let Some(id) = rec.active_export() {
        return Ok(id);
    }
    let id = sink.export_sync(rec.counter)?;
    rec.shared = Some(id);
    Ok(id)
}

pub fn import_semaphore(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphore: VkSemaphore,
    export: SyncExportId,
    temporary: bool,
) -> Result<()> {
    let rec = dev
        .semaphores
        .get_mut(&semaphore)
        .ok_or(GpuError::Invalid("unknown VkSemaphore"))?;
    if !rec.timeline {
        return Err(GpuError::Invalid("external semaphore must be timeline"));
    }
    if temporary {
        return Err(GpuError::Invalid(
            "temporary import is invalid for timeline semaphores",
        ));
    }
    sink.import_sync(export)?;
    let slot = &mut rec.shared;
    if let Some(previous) = slot.replace(export) {
        if let Err(error) = sink.release_sync(previous) {
            *slot = Some(previous);
            let _ = sink.release_sync(export);
            return Err(error);
        }
    }
    Ok(())
}

pub fn signal_shared_semaphore(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphore: VkSemaphore,
    value: u64,
) -> Result<()> {
    let rec = dev
        .semaphores
        .get_mut(&semaphore)
        .ok_or(GpuError::Invalid("unknown VkSemaphore"))?;
    if let Some(export) = rec.active_export() {
        sink.signal_sync(export, value)?;
        rec.counter = rec.counter.max(value);
        Ok(())
    } else {
        dev.signal_semaphore(semaphore, value)
    }
}

pub fn wait_shared_semaphore(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphore: VkSemaphore,
    value: u64,
    timeout_ns: u64,
) -> Result<TimelineWait> {
    let rec = dev
        .semaphores
        .get_mut(&semaphore)
        .ok_or(GpuError::Invalid("unknown VkSemaphore"))?;
    let Some(export) = rec.active_export() else {
        return Ok(if rec.counter >= value {
            TimelineWait::Reached
        } else {
            TimelineWait::Timeout
        });
    };
    let status = sink.wait_sync(export, value, timeout_ns)?;
    if status == TimelineWait::Reached {
        rec.counter = rec.counter.max(value);
    }
    Ok(status)
}

pub fn shared_semaphore_counter(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphore: VkSemaphore,
) -> Result<u64> {
    let rec = dev
        .semaphores
        .get_mut(&semaphore)
        .ok_or(GpuError::Invalid("unknown VkSemaphore"))?;
    if !rec.timeline {
        return Err(GpuError::Invalid(
            "semaphore counter requires timeline semaphore",
        ));
    }
    if let Some(export) = rec.active_export() {
        rec.counter = sink.query_sync(export)?;
    }
    Ok(rec.counter)
}

pub fn wait_shared_semaphores(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphores: &[VkSemaphore],
    values: &[u64],
    any: bool,
    timeout_ns: u64,
) -> Result<TimelineWait> {
    let count = semaphores.len().min(values.len());
    if count == 0 {
        return Ok(TimelineWait::Reached);
    }
    let deadline =
        std::time::Instant::now().checked_add(std::time::Duration::from_nanos(timeout_ns));
    loop {
        let mut reached = 0usize;
        for index in 0..count {
            if wait_shared_semaphore(dev, sink, semaphores[index], values[index], 0)?
                == TimelineWait::Reached
            {
                reached += 1;
            }
        }
        if (any && reached != 0) || (!any && reached == count) {
            return Ok(TimelineWait::Reached);
        }
        if timeout_ns == 0 || deadline.is_none_or(|at| std::time::Instant::now() >= at) {
            return Ok(TimelineWait::Timeout);
        }
        std::thread::sleep(std::time::Duration::from_micros(100));
    }
}

pub fn destroy_shared_semaphore(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    semaphore: VkSemaphore,
) {
    if let Some(rec) = dev.semaphores.remove(&semaphore) {
        if let Some(id) = rec.shared {
            let _ = sink.release_sync(id);
        }
    }
}

// ---- semaphores (binary + timeline) --------------------------------------------------------------

/// `vkCreateSemaphore` — mint a semaphore. `timeline` selects `VK_SEMAPHORE_TYPE_TIMELINE` (with
/// `initial` counter); otherwise a binary semaphore. No IR (present/acquire sync is bookkeeping in the
/// synchronous executor; the timeline counter is host-visible state).
pub fn create_semaphore(dev: &mut Device, timeline: bool, initial: u64) -> VkSemaphore {
    let handle = dev.alloc_handle();
    let rec = if timeline {
        SemaphoreRec::timeline(initial)
    } else {
        SemaphoreRec::binary()
    };
    dev.semaphores.insert(handle, rec);
    handle
}

/// `vkDestroySemaphore` — drop the semaphore (no-op on unknown / `VK_NULL_HANDLE`).
impl Device {
    pub fn destroy_semaphore(&mut self, semaphore: VkSemaphore) {
        self.semaphores.remove(&semaphore);
    }

    /// `vkSignalSemaphore` — host signal of a TIMELINE semaphore to `value` (the counter only advances, so
    /// it moves to `max(counter, value)`). Errors on unknown or a binary semaphore. Ported from
    /// `MVKTimelineSemaphore::signal`.
    pub fn signal_semaphore(&mut self, semaphore: VkSemaphore, value: u64) -> Result<()> {
        match self.semaphores.get_mut(&semaphore) {
            Some(sm) if sm.timeline => {
                sm.counter = sm.counter.max(value);
                Ok(())
            }
            _ => Err(GpuError::Invalid(
                "vkSignalSemaphore: unknown or non-timeline VkSemaphore",
            )),
        }
    }

    /// `vkGetSemaphoreCounterValue` — the current counter of a TIMELINE semaphore. Errors on unknown or a
    /// binary semaphore.
    pub fn semaphore_counter(&self, semaphore: VkSemaphore) -> Result<u64> {
        match self.semaphores.get(&semaphore) {
            Some(sm) if sm.timeline => Ok(sm.counter),
            _ => Err(GpuError::Invalid(
                "vkGetSemaphoreCounterValue: unknown or non-timeline VkSemaphore",
            )),
        }
    }
}

/// `vkWaitSemaphores` — whether the timeline wait is satisfied NOW (`any` = `VK_SEMAPHORE_WAIT_ANY_BIT`,
/// else wait-all). The host signals are synchronous, so a satisfied wait returns `true` immediately and
/// an unsatisfiable one returns `false` (the shim maps that to `VK_TIMEOUT`; it never blocks on a counter
/// that cannot advance). An unknown semaphore counts as unreached. Ported from `MVKDevice::waitSemaphores`.
pub fn wait_semaphores(
    dev: &Device,
    semaphores: &[VkSemaphore],
    values: &[u64],
    any: bool,
) -> bool {
    if semaphores.is_empty() {
        return true;
    }
    let reached = |i: usize| {
        dev.semaphores
            .get(&semaphores[i])
            .map(|sm| sm.counter >= values[i])
            .unwrap_or(false)
    };
    let n = semaphores.len().min(values.len());
    if any {
        (0..n).any(reached)
    } else {
        (0..n).all(reached)
    }
}

// ---- query pools ---------------------------------------------------------------------------------

/// `vkCreateQueryPool` — mint a pool of `count` unavailable/zero slots of `query_type`. Errors on a
/// zero-count pool (a usage error). No IR.
pub fn create_query_pool(dev: &mut Device, query_type: i32, count: u32) -> Result<VkQueryPool> {
    if count == 0 {
        return Err(GpuError::Invalid(
            "vkCreateQueryPool: queryCount must be > 0",
        ));
    }
    let handle = dev.alloc_handle();
    dev.query_pools
        .insert(handle, QueryPoolRec::new(query_type, count));
    Ok(handle)
}

/// `vkDestroyQueryPool` — drop the pool (no-op on unknown / `VK_NULL_HANDLE`).
impl Device {
    pub fn destroy_query_pool(&mut self, pool: VkQueryPool) {
        self.query_pools.remove(&pool);
    }
}

/// `vkResetQueryPool` (Vulkan 1.2 / `VK_EXT_host_query_reset`) — host reset of `[first, first+count)` to
/// unavailable/zero (no command buffer). Ported from `query.rs::vkResetQueryPool`.
pub fn reset_query_pool(dev: &mut Device, pool: VkQueryPool, first: u32, count: u32) {
    if let Some(p) = dev.query_pools.get_mut(&pool) {
        for i in first..first.saturating_add(count) {
            if let Some(slot) = p.results.get_mut(i as usize) {
                *slot = QueryResult::default();
            }
        }
    }
}

/// `vkGetQueryPoolResults` — copy `[first, first+count)` result slots into `out` honouring the flag set.
/// Returns `Ok(true)` when every requested slot was written (`VK_SUCCESS`), `Ok(false)` when at least one
/// was unavailable and neither `wait` nor `partial` was set (`VK_NOT_READY`). Errors (analogous to
/// `VK_ERROR_INITIALIZATION_FAILED`) on an unknown pool or an out-of-range span. Ported from
/// `query.rs::vkGetQueryPoolResults` / `MVKQueryPool::getResults`.
#[allow(clippy::too_many_arguments)]
pub fn get_query_pool_results(
    dev: &Device,
    pool: VkQueryPool,
    first: u32,
    count: u32,
    out: &mut [u8],
    stride: u64,
    wide: bool,
    wait: bool,
    with_availability: bool,
    partial: bool,
) -> Result<bool> {
    let p = dev.query_pools.get(&pool).ok_or(GpuError::Invalid(
        "vkGetQueryPoolResults: unknown VkQueryPool",
    ))?;
    if first.checked_add(count).is_none_or(|end| end > p.count) {
        return Err(GpuError::Invalid(
            "vkGetQueryPoolResults: query range out of bounds",
        ));
    }
    let elem = if wide { 8usize } else { 4usize };
    let len = out.len();
    let mut all_ready = true;
    for i in 0..count as usize {
        let slot = p.results[first as usize + i];
        // u64 checked math: a hostile `stride` near `u64::MAX` must not overflow `i * stride` (an
        // out-of-`out` element is simply skipped, matching the in-range `base + elem <= len` guard).
        let base = (i as u64)
            .checked_mul(stride)
            .and_then(|b| usize::try_from(b).ok());
        // Availability gates the value write unless the caller opted into WAIT (satisfied immediately in
        // the synchronous model) or PARTIAL (write whatever is there). Missing availability → not ready.
        if slot.available || wait || partial {
            if let Some(base) = base {
                if base.checked_add(elem).is_some_and(|end| end <= len) {
                    write_le(out, base, slot.value, wide);
                }
            }
        } else {
            all_ready = false;
        }
        if with_availability {
            if let Some(a) = base.and_then(|b| b.checked_add(elem)) {
                if a.checked_add(elem).is_some_and(|end| end <= len) {
                    write_le(out, a, slot.available as u64, wide);
                }
            }
        }
    }
    Ok(all_ready)
}

/// Write a query result value into `buf` at `off`, 32- or 64-bit little-endian per `wide`.
fn write_le(buf: &mut [u8], off: usize, v: u64, wide: bool) {
    if wide {
        if let Some(d) = buf.get_mut(off..off + 8) {
            d.copy_from_slice(&v.to_le_bytes());
        }
    } else if let Some(d) = buf.get_mut(off..off + 4) {
        d.copy_from_slice(&(v as u32).to_le_bytes());
    }
}
