//! `vkQueueSubmit` / `vkWaitForFences` — the queue submission + host-blocking sync lowering.
//!
//! Ported from `hl-shim-vk/src/command.rs::vkQueueSubmit`. A submit (1) flushes every persistently-
//! mapped HOST_COHERENT buffer as a [`Cmd::WriteBuffer`] (the vkcube per-frame uniform pattern), then
//! (2) ships each submitted command buffer's recorded encoder as one [`Cmd::Submit`], optionally
//! signalling a `VkFence` on the last one. The whole frame goes out as a single sink batch. A fence
//! wait lowers to a real [`CommandSink::wait`] on the fence's timeline value (the same barrier shape
//! hl-cuda's `synchronize` uses).

use crate::model::command::CommandBufferState;
use crate::model::sync::{DeferredOp, QueryResult};
use crate::*;
use hl_gpu::{Cmd, CommandBuffer, CommandSink, FenceId, GpuError, Result};

/// `vkQueueSubmit` — flush mapped memory, then submit each command buffer's encoder as a `Cmd::Submit`;
/// if `signal_fence` is given, the last submit signals it at a fresh timeline value. All command
/// buffers move to `Pending`. Errors on an unknown command-buffer / fence handle.
pub fn queue_submit(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    command_buffers: &[VkCommandBuffer],
    signal_fence: Option<VkFence>,
) -> Result<()> {
    let _submit_span = hl_log::hl_span!(hl_log::tag::VULKAN, "submit");
    // Resolve the fence signal (ir id + a fresh monotonic timeline value) up front.
    let signal = match signal_fence {
        Some(f) => {
            let ir = dev
                .fences
                .get(&f)
                .ok_or(GpuError::Invalid("vkQueueSubmit: unknown VkFence"))?
                .ir_id;
            let value = dev.next_fence_value();
            Some((f, ir, value))
        }
        None => None,
    };

    // Snapshot each command buffer's encoder + its out-of-band buffer writes + deferred device ops
    // (owned), validating the handle + state.
    let mut encoders: Vec<Vec<hl_gpu::Enc>> = Vec::with_capacity(command_buffers.len());
    let mut buffer_writes: Vec<(u32, u64, Vec<u8>)> = Vec::new();
    let mut deferred: Vec<DeferredOp> = Vec::new();
    for &cb in command_buffers {
        let rec = dev
            .command_buffers
            .get(&cb)
            .ok_or(GpuError::Invalid("vkQueueSubmit: unknown VkCommandBuffer"))?;
        if rec.state != CommandBufferState::Executable {
            hl_log::hl_warn!(
                hl_log::tag::VULKAN,
                "submit not-executable cb={cb:#x} state={:?}",
                rec.state
            );
            return Err(GpuError::Invalid(
                "vkQueueSubmit: command buffer is not executable",
            ));
        }
        encoders.push(rec.enc.clone());
        buffer_writes.extend(rec.buffer_writes.iter().cloned());
        deferred.extend(rec.deferred.iter().cloned());
    }

    // Build the frame: mapped-buffer flushes first, then the recorded fill/update buffer writes, then one
    // Submit per command buffer.
    let mut batch: Vec<Cmd> = Vec::new();
    for (id, offset, data) in dev.mapped_uploads() {
        batch.push(Cmd::WriteBuffer { id, offset, data });
    }
    for (id, offset, data) in buffer_writes {
        batch.push(Cmd::WriteBuffer { id, offset, data });
    }
    let last = encoders.len().saturating_sub(1);
    if encoders.is_empty() {
        // A fence-only submit (no command buffers) still signals the fence via an empty command buffer.
        if let Some((_, ir, value)) = signal {
            batch.push(Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: Some((ir, value)),
            }));
        }
    } else {
        for (i, encoder) in encoders.into_iter().enumerate() {
            let sig = if i == last {
                signal.map(|(_, ir, value)| (ir, value))
            } else {
                None
            };
            batch.push(Cmd::Submit(CommandBuffer {
                encoder,
                signal: sig,
            }));
        }
    }

    // Apply the deferred device ops (events set/reset, query reset/end/timestamp) — the host replay is
    // synchronous, so they resolve at submit completion. `vkCmdCopyQueryPoolResults` reads the resolved
    // pool state and appends its result copy as a trailing `Cmd::WriteBuffer` in the same frame.
    for op in deferred {
        if let Some(write) = dev.apply_deferred(op) {
            batch.push(write);
        }
    }

    hl_log::hl_debug!(
        hl_log::tag::VULKAN,
        "submit cbs={} cmds={} fence={}",
        command_buffers.len(),
        batch.len(),
        signal.is_some()
    );
    hl_log::hl_count!(hl_log::tag::VULKAN, "submits");
    hl_log::hl_add!(hl_log::tag::VULKAN, "cmds", batch.len() as u64);
    sink.submit(&batch)?;

    // The captured pending host→device uploads (from vkUnmapMemory / vkFlushMappedMemoryRanges) have now
    // reached the device in this frame — retire them so they are not re-flushed on the next submit.
    dev.clear_pending_uploads();

    // Advance model state. The host replay is SYNCHRONOUS — `sink.submit` above already ran every command
    // buffer to completion — so each buffer's execution is done on return. Per Vulkan §6.4, a completed
    // buffer recorded WITHOUT `ONE_TIME_SUBMIT` returns to `Executable` (re-submittable); a one-time buffer
    // becomes non-resubmittable. Leaving a re-submittable buffer stuck in `Pending` would fail the NEXT
    // `vkQueueSubmit` of that same buffer ("not executable") — exactly the vkcube per-image draw loop, which
    // records each swapchain image's command buffer once and re-submits it every frame.
    for &cb in command_buffers {
        if let Some(rec) = dev.command_buffers.get_mut(&cb) {
            rec.state = if rec.one_time_submit {
                CommandBufferState::Pending
            } else {
                CommandBufferState::Executable
            };
        }
    }
    if let Some((f, _, value)) = signal {
        if let Some(fence) = dev.fences.get_mut(&f) {
            fence.value = value;
            fence.signaled = false;
        }
    }
    Ok(())
}

/// Wait for a device fence through the host command sink.
///
/// This free function is the stable service boundary for callers that do not
/// own a concrete [`Device`] implementation.
pub fn wait_for_fence(dev: &mut Device, sink: &mut dyn CommandSink, fence: VkFence) -> Result<()> {
    Device::wait_for_fence(dev, sink, fence)
}

/// Apply a batch's queue-side timeline signals (`VkTimelineSemaphoreSubmitInfo::pSignalSemaphoreValues`
/// / sync2 `VkSemaphoreSubmitInfo::value`) AFTER [`queue_submit`] has returned. The host replay is
/// SYNCHRONOUS, so the producer's command buffers have fully executed by the time this runs — advancing
/// each signalled TIMELINE semaphore's monotonic counter makes the timeline a truthful, already-satisfied
/// ordering point: a subsequent `vkWaitSemaphores(>= value)` or a consumer submit waiting on it observes
/// the producer's result. A binary or unknown semaphore in the set is skipped (a binary semaphore carries
/// no counter, and a real driver ignores its supplied value). `VK_KHR_timeline_semaphore`.
impl Device {
    pub fn signal_timeline_values(&mut self, signals: &[(VkSemaphore, u64)]) {
        for &(sem, value) in signals {
            if let Some(sm) = self.semaphores.get_mut(&sem) {
                if sm.timeline {
                    sm.counter = sm.counter.max(value);
                }
            }
        }
    }

    /// `vkWaitForFences` (one fence) — block on the fence's timeline value via [`CommandSink::wait`], then
    /// mark it signaled. Errors on an unknown fence. A never-submitted fence (value 0) waits on 0 (already
    /// satisfied) — matching a real driver returning immediately for an unsignalled-but-idle fence only
    /// once armed; here the guest-side `signaled` flag is the observable state.
    pub fn wait_for_fence(
        dev: &mut Device,
        sink: &mut dyn CommandSink,
        fence: VkFence,
    ) -> Result<()> {
        let (ir, value) = {
            let f = dev
                .fences
                .get(&fence)
                .ok_or(GpuError::Invalid("vkWaitForFences: unknown VkFence"))?;
            (f.ir_id, f.value)
        };
        sink.wait(FenceId(ir), value)?;
        dev.fences.get_mut(&fence).unwrap().signaled = true;
        Ok(())
    }

    /// Apply one recorded device event/query op to the device state at (synchronous) submit completion,
    /// returning a `Cmd::WriteBuffer` for the one op (`CopyResults`) that emits IR. Ported from
    /// `query.rs::apply_deferred`.
    fn apply_deferred(&mut self, op: DeferredOp) -> Option<Cmd> {
        match op {
            DeferredOp::Event { event, set } => {
                if let Some(e) = self.events.get_mut(&event) {
                    e.signaled = set;
                }
                None
            }
            DeferredOp::QueryReset { pool, first, count } => {
                if let Some(p) = self.query_pools.get_mut(&pool) {
                    for i in first..first.saturating_add(count) {
                        if let Some(slot) = p.results.get_mut(i as usize) {
                            *slot = QueryResult::default();
                        }
                    }
                }
                None
            }
            DeferredOp::QueryEnd { pool, query, value } => {
                if let Some(p) = self.query_pools.get_mut(&pool) {
                    if let Some(slot) = p.results.get_mut(query as usize) {
                        slot.available = true;
                        slot.value = value;
                    }
                }
                None
            }
            DeferredOp::QueryTimestamp { pool, query } => {
                let ts = self.next_timestamp();
                if let Some(p) = self.query_pools.get_mut(&pool) {
                    if let Some(slot) = p.results.get_mut(query as usize) {
                        slot.available = true;
                        slot.value = ts;
                    }
                }
                None
            }
            DeferredOp::CopyResults {
                pool,
                first,
                count,
                dst_ir,
                dst_offset,
                dst_size,
                stride,
                wide,
                with_availability,
            } => {
                let p = self.query_pools.get(&pool)?;
                let elem = if wide { 8usize } else { 4usize };
                let mut bytes = vec![0u8; dst_size as usize];
                let len = bytes.len();
                for i in 0..count as usize {
                    let slot = p
                        .results
                        .get(first as usize + i)
                        .copied()
                        .unwrap_or_default();
                    let base = i * stride as usize;
                    if base + elem <= len {
                        write_le(&mut bytes, base, slot.value, wide);
                    }
                    if with_availability {
                        let a = base + elem;
                        if a + elem <= len {
                            write_le(&mut bytes, a, slot.available as u64, wide);
                        }
                    }
                }
                Some(Cmd::WriteBuffer {
                    id: dst_ir,
                    offset: dst_offset,
                    data: bytes,
                })
            }
        }
    }
}

/// Write a query result value into `buf` at `off`, 32- or 64-bit little-endian per `wide`. Out-of-range
/// offsets are silently skipped (the caller has bounds-checked the element span). Ported from
/// `query.rs::write_le`.
fn write_le(buf: &mut [u8], off: usize, v: u64, wide: bool) {
    if wide {
        if let Some(d) = buf.get_mut(off..off + 8) {
            d.copy_from_slice(&v.to_le_bytes());
        }
    } else if let Some(d) = buf.get_mut(off..off + 4) {
        d.copy_from_slice(&(v as u32).to_le_bytes());
    }
}
