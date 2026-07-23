use super::*;

/// `vkCmdPipelineBarrier` / `vkCmdPipelineBarrier2` records image-layout bookkeeping. The neutral IR is
/// layout-implicit, so the executor needs no encoder operation for this transition.
pub fn cmd_pipeline_barrier(
    dev: &mut Device,
    cb: VkCommandBuffer,
    transitions: &[(VkImage, i32, i32)],
) -> Result<()> {
    // The barrier is only valid while recording; validate the buffer state (records/emits nothing else).
    let _ = dev.require_recording(cb)?;
    for &(image, _old, new_layout) in transitions {
        if dev.images.contains_key(&image) {
            dev.image_layouts.insert(image, new_layout);
        }
    }
    Ok(())
}

// ---- events (device set/reset, applied at submit completion) ------------------------------------

/// `vkCmdSetEvent` / `vkCmdResetEvent` — record a device set/reset of `event`, applied at (synchronous)
/// submit completion. Errors on an unknown event. Ported from `event.rs::cmd_event`.
pub fn cmd_set_event(
    dev: &mut Device,
    cb: VkCommandBuffer,
    event: VkEvent,
    set: bool,
) -> Result<()> {
    if !dev.events.contains_key(&event) {
        return Err(GpuError::Invalid(
            "vkCmdSetEvent/ResetEvent: unknown VkEvent",
        ));
    }
    dev.require_recording(cb)?
        .deferred
        .push(DeferredOp::Event { event, set });
    Ok(())
}

/// `vkCmdWaitEvents` — validate the waited events all exist (a wait on an unknown event is a usage
/// error). In this synchronous single-queue model the waited dependency has already resolved by submit
/// completion, so the wait records no op. Ported from `event.rs::vkCmdWaitEvents`.
pub fn cmd_wait_events(dev: &mut Device, cb: VkCommandBuffer, events: &[VkEvent]) -> Result<()> {
    let _ = dev.require_recording(cb)?;
    if !events.iter().all(|e| dev.events.contains_key(e)) {
        return Err(GpuError::Invalid("vkCmdWaitEvents: unknown VkEvent"));
    }
    Ok(())
}

// ---- queries (device reset/begin/end/timestamp/copy, applied at submit completion) --------------

/// `vkCmdResetQueryPool` — record a device reset of `[first, first+count)` (applied at completion).
/// Errors on an unknown pool or out-of-range span. Ported from `query.rs::vkCmdResetQueryPool`.
pub fn cmd_reset_query_pool(
    dev: &mut Device,
    cb: VkCommandBuffer,
    pool: VkQueryPool,
    first: u32,
    count: u32,
) -> Result<()> {
    match dev.query_pools.get(&pool) {
        Some(p) if first.checked_add(count).is_some_and(|e| e <= p.count) => {}
        _ => {
            return Err(GpuError::Invalid(
                "vkCmdResetQueryPool: unknown pool or out-of-range span",
            ))
        }
    }
    dev.require_recording(cb)?
        .deferred
        .push(DeferredOp::QueryReset { pool, first, count });
    Ok(())
}

/// `vkCmdBeginQuery` — open `(pool, query)` on the command buffer (spec §17.4: at most one active query
/// of a type; a second open is ignored). Availability is set at `vkCmdEndQuery`. Errors on a bad pool/
/// index. Ported from `query.rs::vkCmdBeginQuery`.
pub fn cmd_begin_query(
    dev: &mut Device,
    cb: VkCommandBuffer,
    pool: VkQueryPool,
    query: u32,
) -> Result<()> {
    // VkQueryType OCCLUSION == 0 — an occlusion query counts the fragments its scope draws pass.
    let is_occlusion = match dev.query_pools.get(&pool) {
        Some(p) if query < p.count => p.query_type == 0,
        _ => {
            return Err(GpuError::Invalid(
                "vkCmdBeginQuery: unknown pool or query index",
            ))
        }
    };
    let rec = dev.require_recording(cb)?;
    if rec.active_query.is_none() {
        rec.active_query = Some((pool, query));
        // Arm the occlusion accumulator so each draw in [begin,end) adds its sample footprint.
        rec.occlusion_accum = if is_occlusion { Some(0) } else { None };
    }
    Ok(())
}

/// `vkCmdEndQuery` — close the matching open query, recording it available at the accumulated OCCLUSION
/// sample count (the scissor-clipped footprint of every draw in the query scope; `0` when nothing
/// rasterized or the draws were fully scissored). A non-occlusion query resolves at `0`. Ported from
/// `query.rs::vkCmdEndQuery`, upgraded from the old conservative-constant `0` to a coverage that reflects
/// reality.
pub fn cmd_end_query(
    dev: &mut Device,
    cb: VkCommandBuffer,
    pool: VkQueryPool,
    query: u32,
) -> Result<()> {
    let rec = dev.require_recording(cb)?;
    if rec.active_query == Some((pool, query)) {
        rec.active_query = None;
        let value = rec.occlusion_accum.take().unwrap_or(0);
        rec.deferred
            .push(DeferredOp::QueryEnd { pool, query, value });
    }
    Ok(())
}

/// `vkCmdWriteTimestamp` — record a timestamp write into `(pool, query)`, resolved to a host-monotonic
/// serial at submit completion. Errors on a bad pool/index. Ported from `query.rs::vkCmdWriteTimestamp`.
pub fn cmd_write_timestamp(
    dev: &mut Device,
    cb: VkCommandBuffer,
    pool: VkQueryPool,
    query: u32,
) -> Result<()> {
    match dev.query_pools.get(&pool) {
        Some(p) if query < p.count => {}
        _ => {
            return Err(GpuError::Invalid(
                "vkCmdWriteTimestamp: unknown pool or query index",
            ))
        }
    }
    dev.require_recording(cb)?
        .deferred
        .push(DeferredOp::QueryTimestamp { pool, query });
    Ok(())
}

/// `vkCmdCopyQueryPoolResults` — record a write of the pool's `[first, first+count)` results into
/// `dst_buffer` at completion (an IR `WriteBuffer`). The destination must be `COPY_DST` and the written
/// span must fit. Ported from `query.rs::vkCmdCopyQueryPoolResults`.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_query_pool_results(
    dev: &mut Device,
    cb: VkCommandBuffer,
    pool: VkQueryPool,
    first: u32,
    count: u32,
    dst_buffer: VkBuffer,
    dst_offset: u64,
    stride: u64,
    wide: bool,
    with_availability: bool,
) -> Result<()> {
    match dev.query_pools.get(&pool) {
        Some(p) if first.checked_add(count).is_some_and(|e| e <= p.count) => {}
        _ => {
            return Err(GpuError::Invalid(
                "vkCmdCopyQueryPoolResults: unknown pool or out-of-range span",
            ))
        }
    }
    let (dst_ir, bsize, usage) = {
        let b = dev.buffers.get(&dst_buffer).ok_or(GpuError::Invalid(
            "vkCmdCopyQueryPoolResults: unknown dst VkBuffer",
        ))?;
        (b.ir_id, b.size, b.usage)
    };
    if usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdCopyQueryPoolResults: dst missing COPY_DST usage",
        ));
    }
    let elem = if wide { 8u64 } else { 4u64 };
    let per = if with_availability { elem * 2 } else { elem };
    // Span from dst_offset through the last written element.
    let span = count
        .checked_sub(1)
        .map(|last| (last as u64).checked_mul(stride))
        .unwrap_or(Some(0))
        .and_then(|off| off.checked_add(per))
        .ok_or(GpuError::OutOfBounds)?;
    // The written region is exactly `span` bytes from `dst_offset`; sizing the copy to `span` (not the
    // looser `count * max(stride, per)`, which can overflow `u64` for a hostile count/stride and then
    // abort the host on a multi-EiB `vec![0u8; dst_size]`) keeps the emitted WriteBuffer inside the
    // bounds validated below.
    let dst_size = span;
    match dst_offset.checked_add(span) {
        Some(end) if end <= bsize => {}
        _ => return Err(GpuError::OutOfBounds),
    }
    dev.require_recording(cb)?
        .deferred
        .push(DeferredOp::CopyResults {
            pool,
            first,
            count,
            dst_ir,
            dst_offset,
            dst_size,
            stride,
            wide,
            with_availability,
        });
    Ok(())
}
