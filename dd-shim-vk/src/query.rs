//! `VkQueryPool` + its device recording commands (real bodies), and the applier for the deferred
//! device query/event ops recorded into a command buffer.
//!
//! Ported from MoltenVK's query objects (`GPUObjects/MVKQueryPool.mm`): `MVKOcclusionQueryPool`,
//! `MVKTimestampQueryPool`, `MVKPipelineStatisticsQueryPool`. A pool is a fixed array of typed slots
//! with per-slot availability (`MVKQueryPool::_availability`). `vkGetQueryPoolResults` reads the slots
//! honouring the `VkQueryResultFlags` (64-bit width, WAIT, WITH_AVAILABILITY, PARTIAL); the recording
//! commands (`vkCmdResetQueryPool`/`vkCmdBeginQuery`/`vkCmdEndQuery`/`vkCmdWriteTimestamp`/
//! `vkCmdCopyQueryPoolResults`) are deferred and applied at the (synchronous) submit completion.
//!
//! Bounded-domain (partial) truthfulness: our host replay is synchronous and does not surface real GPU
//! sample counts, so occlusion / pipeline-statistics results are a conservative `0`; timestamps are a
//! host-monotonic serial (`timestampPeriod`-agnostic). Availability and the read/copy machinery are
//! real and observable through the public ABI.

use crate::reg::{self, DeferredOp, QueryPoolRec, QueryResult, VkState};
use crate::types::*;
use ash::vk;
use core::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

/// Write a query result value into `buf` at `off`, 32- or 64-bit little-endian per `wide`. Out-of-range
/// offsets are silently skipped (the caller has already bounds-checked the element span).
fn write_le(buf: &mut [u8], off: usize, v: u64, wide: bool) {
    if wide {
        if let Some(d) = buf.get_mut(off..off + 8) {
            d.copy_from_slice(&v.to_le_bytes());
        }
    } else if let Some(d) = buf.get_mut(off..off + 4) {
        d.copy_from_slice(&(v as u32).to_le_bytes());
    }
}

/// Monotonic host timestamp source (`vkCmdWriteTimestamp`). Not wall-clock; a strictly increasing
/// serial, which is the only guarantee an app may rely on across two timestamps in submission order.
static TIMESTAMP: AtomicU64 = AtomicU64::new(1);

// VkQueryResultFlagBits (stable ABI).
const VK_QUERY_RESULT_64_BIT: u32 = 0x1;
const VK_QUERY_RESULT_WAIT_BIT: u32 = 0x2;
const VK_QUERY_RESULT_WITH_AVAILABILITY_BIT: u32 = 0x4;
const VK_QUERY_RESULT_PARTIAL_BIT: u32 = 0x8;

/// Apply one recorded device query/event op to the global state on submit completion. Ported from the
/// point in `MVKQueue` where an encoded query resolves and its availability + value become visible.
pub fn apply_deferred(s: &mut VkState, op: DeferredOp) {
    match op {
        DeferredOp::QueryReset { pool, first, count } => {
            if let Some(p) = s.query_pools.get_mut(&pool) {
                for i in first..first.saturating_add(count) {
                    if let Some(slot) = p.results.get_mut(i as usize) {
                        *slot = QueryResult::default();
                    }
                }
            }
        }
        DeferredOp::QueryEnd { pool, query, value } => {
            if let Some(p) = s.query_pools.get_mut(&pool) {
                if let Some(slot) = p.results.get_mut(query as usize) {
                    slot.available = true;
                    slot.value = value;
                }
            }
        }
        DeferredOp::QueryTimestamp { pool, query } => {
            let ts = TIMESTAMP.fetch_add(1, Ordering::Relaxed);
            if let Some(p) = s.query_pools.get_mut(&pool) {
                if let Some(slot) = p.results.get_mut(query as usize) {
                    slot.available = true;
                    slot.value = ts;
                }
            }
        }
        DeferredOp::Event { event, set } => {
            if let Some(e) = s.events.get_mut(&event) {
                e.signaled = set;
            }
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
            let Some(p) = s.query_pools.get(&pool) else { return };
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
            s.ir_log.push(dd_shim_common::ir::Cmd::WriteBuffer { id: dst_ir, offset: dst_offset, data: bytes });
        }
    }
}

// ---- host object lifecycle -----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateQueryPool(
    _device: VkDevice,
    p_create_info: *const vk::QueryPoolCreateInfo,
    _p_allocator: *const c_void,
    p_query_pool: *mut VkQueryPool,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_query_pool.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if ci.query_count == 0 {
        unsafe { *(p_query_pool as *mut u64) = 0 };
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.query_pools.insert(
        handle,
        QueryPoolRec {
            query_type: ci.query_type.as_raw(),
            count: ci.query_count,
            results: vec![QueryResult::default(); ci.query_count as usize],
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyQueryPool(_device: VkDevice, query_pool: VkQueryPool, _p_allocator: *const c_void) {
    reg::lock().query_pools.remove(&query_pool);
}

/// `vkGetQueryPoolResults` — copy the pool's `[firstQuery, firstQuery+queryCount)` slots into the
/// caller's buffer honouring `VkQueryResultFlags`. Returns `VK_NOT_READY` if any requested query is
/// unavailable and neither `WAIT` nor `PARTIAL` was set (spec §17.5). Ported from
/// `MVKQueryPool::getResults`.
#[no_mangle]
pub extern "C" fn vkGetQueryPoolResults(
    _device: VkDevice,
    query_pool: VkQueryPool,
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
    let s = reg::lock();
    let Some(pool) = s.query_pools.get(&query_pool) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if first_query.checked_add(query_count).is_none_or(|end| end > pool.count) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let wide = flags & VK_QUERY_RESULT_64_BIT != 0;
    let wait = flags & VK_QUERY_RESULT_WAIT_BIT != 0;
    let with_avail = flags & VK_QUERY_RESULT_WITH_AVAILABILITY_BIT != 0;
    let partial = flags & VK_QUERY_RESULT_PARTIAL_BIT != 0;
    let elem = if wide { 8usize } else { 4usize };
    let out = unsafe { core::slice::from_raw_parts_mut(p_data as *mut u8, data_size) };
    let len = out.len();
    let mut overall = VK_SUCCESS;
    for i in 0..query_count as usize {
        let slot = pool.results[first_query as usize + i];
        let base = i * stride as usize;
        // Availability gates the value write unless the caller opted into WAIT (block-until-ready —
        // synchronous here) or PARTIAL (write whatever is there). Missing availability → VK_NOT_READY.
        if slot.available || wait || partial {
            if base + elem <= len {
                write_le(out, base, slot.value, wide);
            }
        } else {
            overall = VK_NOT_READY;
        }
        if with_avail {
            let a = base + elem;
            if a + elem <= len {
                write_le(out, a, slot.available as u64, wide);
            }
        }
    }
    overall
}

// ---- device recording commands -------------------------------------------------------------------

/// Record a deferred op into a recording command buffer (dropped if not recording — the same gate the
/// `vkCmd*` bodies use).
fn record(cb: VkCommandBuffer, op: DeferredOp) {
    let mut s = reg::lock();
    if let Some(rec) = s.recording_mut(cb as usize) {
        rec.deferred.push(op);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdResetQueryPool(
    command_buffer: VkCommandBuffer,
    query_pool: VkQueryPool,
    first_query: u32,
    query_count: u32,
) {
    // Validate the pool + range before recording (a bad range records nothing).
    {
        let s = reg::lock();
        match s.query_pools.get(&query_pool) {
            Some(p) if first_query.checked_add(query_count).is_some_and(|e| e <= p.count) => {}
            _ => return,
        }
    }
    record(command_buffer, DeferredOp::QueryReset { pool: query_pool, first: first_query, count: query_count });
}

#[no_mangle]
pub extern "C" fn vkCmdBeginQuery(
    command_buffer: VkCommandBuffer,
    query_pool: VkQueryPool,
    query: u32,
    _flags: vk::QueryControlFlags,
) {
    {
        let s = reg::lock();
        match s.query_pools.get(&query_pool) {
            Some(p) if query < p.count => {}
            _ => return,
        }
    }
    // Track the open (pool, query) so vkCmdEndQuery resolves the matching slot (spec §17.4: at most one
    // active query of a type). We do not defer a Begin op — availability is set at End.
    let mut s = reg::lock();
    if let Some(rec) = s.recording_mut(command_buffer as usize) {
        if rec.active_query.is_none() {
            rec.active_query = Some((query_pool, query));
        }
    }
}

#[no_mangle]
pub extern "C" fn vkCmdEndQuery(command_buffer: VkCommandBuffer, query_pool: VkQueryPool, query: u32) {
    let mut s = reg::lock();
    if let Some(rec) = s.recording_mut(command_buffer as usize) {
        if rec.active_query == Some((query_pool, query)) {
            rec.active_query = None;
            // Synchronous model: no real GPU sample count is available, so the occlusion / statistics
            // result is a conservative 0 (bounded — see the module note). Availability is real.
            rec.deferred.push(DeferredOp::QueryEnd { pool: query_pool, query, value: 0 });
        }
    }
}

#[no_mangle]
pub extern "C" fn vkCmdWriteTimestamp(
    command_buffer: VkCommandBuffer,
    _pipeline_stage: vk::PipelineStageFlags,
    query_pool: VkQueryPool,
    query: u32,
) {
    {
        let s = reg::lock();
        match s.query_pools.get(&query_pool) {
            Some(p) if query < p.count => {}
            _ => return,
        }
    }
    record(command_buffer, DeferredOp::QueryTimestamp { pool: query_pool, query });
}

/// `vkCmdCopyQueryPoolResults` — write the pool results into a destination buffer on (synchronous)
/// completion. Bounded: the results are our synchronous model's values (see module note).
#[no_mangle]
pub extern "C" fn vkCmdCopyQueryPoolResults(
    command_buffer: VkCommandBuffer,
    query_pool: VkQueryPool,
    first_query: u32,
    query_count: u32,
    dst_buffer: VkBuffer,
    dst_offset: u64,
    stride: u64,
    flags: u32,
) {
    let mut s = reg::lock();
    // Validate: pool + range, destination buffer exists with TRANSFER_DST usage, and the written span
    // fits inside the buffer.
    match s.query_pools.get(&query_pool) {
        Some(p) if first_query.checked_add(query_count).is_some_and(|e| e <= p.count) => {}
        _ => return,
    }
    let Some(buf) = s.buffers.get(&dst_buffer) else { return };
    if buf.usage & dd_shim_common::ir::buffer_usage::COPY_DST == 0 {
        return;
    }
    let wide = flags & VK_QUERY_RESULT_64_BIT != 0;
    let with_availability = flags & VK_QUERY_RESULT_WITH_AVAILABILITY_BIT != 0;
    let elem = if wide { 8u64 } else { 4u64 };
    let per = if with_availability { elem * 2 } else { elem };
    // Span from dst_offset through the last written element.
    let span = match query_count
        .checked_sub(1)
        .map(|last| (last as u64).checked_mul(stride))
        .unwrap_or(Some(0))
        .and_then(|off| off.checked_add(per))
    {
        Some(v) => v,
        None => return,
    };
    let dst_size = query_count as u64 * stride.max(per);
    match dst_offset.checked_add(span) {
        Some(end) if end <= buf.size => {}
        _ => return,
    }
    let (dst_ir, dst_offset2) = (buf.ir_id, dst_offset);
    if let Some(rec) = s.recording_mut(command_buffer as usize) {
        rec.deferred.push(DeferredOp::CopyResults {
            pool: query_pool,
            first: first_query,
            count: query_count,
            dst_ir,
            dst_offset: dst_offset2,
            dst_size,
            stride,
            wide,
            with_availability,
        });
    }
}

/// `vkResetQueryPool` (Vulkan 1.2 / VK_EXT_host_query_reset): host-side reset of a query pool's
/// `[firstQuery, firstQuery+queryCount)` slots to unavailable/zero (no command buffer). Ported from
/// `MVKQueryPool::resetResults`.
#[no_mangle]
pub extern "C" fn vkResetQueryPool(
    _device: VkDevice,
    query_pool: VkQueryPool,
    first_query: u32,
    query_count: u32,
) {
    let mut s = reg::lock();
    if let Some(p) = s.query_pools.get_mut(&query_pool) {
        for i in first_query..first_query.saturating_add(query_count) {
            if let Some(slot) = p.results.get_mut(i as usize) {
                *slot = reg::QueryResult::default();
            }
        }
    }
}

/// `vkResetQueryPoolEXT` (VK_EXT_host_query_reset alias for `vkResetQueryPool`).
#[no_mangle]
pub extern "C" fn vkResetQueryPoolEXT(device: VkDevice, query_pool: VkQueryPool, first_query: u32, query_count: u32) {
    vkResetQueryPool(device, query_pool, first_query, query_count);
}
