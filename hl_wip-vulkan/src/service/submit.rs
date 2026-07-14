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
            return Err(GpuError::Invalid("vkQueueSubmit: command buffer is not executable"));
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
            batch.push(Cmd::Submit(CommandBuffer { encoder: Vec::new(), signal: Some((ir, value)) }));
        }
    } else {
        for (i, encoder) in encoders.into_iter().enumerate() {
            let sig = if i == last { signal.map(|(_, ir, value)| (ir, value)) } else { None };
            batch.push(Cmd::Submit(CommandBuffer { encoder, signal: sig }));
        }
    }

    // Apply the deferred device ops (events set/reset, query reset/end/timestamp) — the host replay is
    // synchronous, so they resolve at submit completion. `vkCmdCopyQueryPoolResults` reads the resolved
    // pool state and appends its result copy as a trailing `Cmd::WriteBuffer` in the same frame.
    for op in deferred {
        if let Some(write) = apply_deferred(dev, op) {
            batch.push(write);
        }
    }

    sink.submit(&batch)?;

    // Advance model state: command buffers pending, fence armed at its signal value.
    for &cb in command_buffers {
        if let Some(rec) = dev.command_buffers.get_mut(&cb) {
            rec.state = CommandBufferState::Pending;
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

/// `vkWaitForFences` (one fence) — block on the fence's timeline value via [`CommandSink::wait`], then
/// mark it signaled. Errors on an unknown fence. A never-submitted fence (value 0) waits on 0 (already
/// satisfied) — matching a real driver returning immediately for an unsignalled-but-idle fence only
/// once armed; here the guest-side `signaled` flag is the observable state.
pub fn wait_for_fence(dev: &mut Device, sink: &mut dyn CommandSink, fence: VkFence) -> Result<()> {
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

/// `vkGetFenceStatus` — the fence's current guest-side signaled state (`true` → `VK_SUCCESS`, `false`
/// → `VK_NOT_READY`). A non-blocking poll of the same `signaled` flag `vkWaitForFences`/`vkResetFences`
/// drive. Errors on an unknown fence.
pub fn fence_status(dev: &Device, fence: VkFence) -> Result<bool> {
    Ok(dev
        .fences
        .get(&fence)
        .ok_or(GpuError::Invalid("vkGetFenceStatus: unknown VkFence"))?
        .signaled)
}

/// `vkResetFences` (one fence) — clear the fence's signaled state.
pub fn reset_fence(dev: &mut Device, fence: VkFence) -> Result<()> {
    dev.fences
        .get_mut(&fence)
        .ok_or(GpuError::Invalid("vkResetFences: unknown VkFence"))?
        .signaled = false;
    Ok(())
}

/// Apply one recorded device event/query op to the device state at (synchronous) submit completion,
/// returning a `Cmd::WriteBuffer` for the one op (`CopyResults`) that emits IR. Ported from
/// `query.rs::apply_deferred`.
fn apply_deferred(dev: &mut Device, op: DeferredOp) -> Option<Cmd> {
    match op {
        DeferredOp::Event { event, set } => {
            if let Some(e) = dev.events.get_mut(&event) {
                e.signaled = set;
            }
            None
        }
        DeferredOp::QueryReset { pool, first, count } => {
            if let Some(p) = dev.query_pools.get_mut(&pool) {
                for i in first..first.saturating_add(count) {
                    if let Some(slot) = p.results.get_mut(i as usize) {
                        *slot = QueryResult::default();
                    }
                }
            }
            None
        }
        DeferredOp::QueryEnd { pool, query, value } => {
            if let Some(p) = dev.query_pools.get_mut(&pool) {
                if let Some(slot) = p.results.get_mut(query as usize) {
                    slot.available = true;
                    slot.value = value;
                }
            }
            None
        }
        DeferredOp::QueryTimestamp { pool, query } => {
            let ts = dev.next_timestamp();
            if let Some(p) = dev.query_pools.get_mut(&pool) {
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
            let p = dev.query_pools.get(&pool)?;
            let elem = if wide { 8usize } else { 4usize };
            let mut bytes = vec![0u8; dst_size as usize];
            let len = bytes.len();
            for i in 0..count as usize {
                let slot = p.results.get(first as usize + i).copied().unwrap_or_default();
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
            Some(Cmd::WriteBuffer { id: dst_ir, offset: dst_offset, data: bytes })
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
