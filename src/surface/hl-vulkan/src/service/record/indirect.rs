use super::*;

/// `vkCmdPushConstants` — retain aligned push-constant bytes as command state. The current IR has no
/// push-constant channel, so the values remain available for a future per-draw uniform lowering.
pub fn cmd_push_constants(
    dev: &mut Device,
    cb: VkCommandBuffer,
    offset: u32,
    bytes: &[u8],
) -> Result<()> {
    if !offset.is_multiple_of(4) || bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(GpuError::Invalid(
            "vkCmdPushConstants: offset/size must be nonzero and 4-byte aligned",
        ));
    }
    // `offset + size` must lie within `maxPushConstantsSize` (spec §17.1, VUID-vkCmdPushConstants-offset).
    // Without this a hostile `offset`/`size` near `u32::MAX` would `resize` the block to multiple GiB and
    // abort the host on the allocation — reject it as a truthful usage error instead.
    let max_push = dev.physical_device.limits.max_push_constants_size as u64;
    if offset as u64 + bytes.len() as u64 > max_push {
        return Err(GpuError::Invalid(
            "vkCmdPushConstants: offset+size exceeds maxPushConstantsSize",
        ));
    }
    let rec = dev.require_recording(cb)?;
    let end = offset as usize + bytes.len();
    if rec.push_constants.len() < end {
        rec.push_constants.resize(end, 0);
    }
    rec.push_constants[offset as usize..end].copy_from_slice(bytes);
    Ok(())
}

// ---- indirect draws / dispatch -----------------------------------------------------------------
// The indirect commands read their draw arguments from a device buffer at execution time. The hl-GPU IR
// carries no indirect encoder op, BUT the argument buffer is host-visible unified memory (every hl device
// buffer is MAP-able) whose bytes the shim already holds in `MemRec::data`. So an indirect DRAW whose
// argument buffer was filled on the CPU before it was recorded (the overwhelmingly common case — a
// mapped/HOST_COHERENT `VkDrawIndirectCommand[]`) is resolved HERE: the shim reads the argument words out
// of the bound allocation and lowers each to the SAME direct `Enc::Draw` / `Enc::DrawIndexed` the
// equivalent `vkCmdDraw` would emit, so an indirect draw and its direct twin rasterize byte-identically.
//
// Honest limits, all documented: the args are snapshotted at RECORD time, so a buffer written by the GPU
// *between* record and submit (e.g. a compute shader that produces the draw args in the same batch) is
// not reflected — that would need a real IR indirect op. An argument buffer that is not (yet) backed by
// bound memory reads as zeros → a `Draw{0,..}` no-op (matching an unwritten buffer). `vkCmdDispatchIndirect`
// resolves its `VkDispatchIndirectCommand{x,y,z}` from the same host-visible backing and lowers to the
// SAME `Enc::Dispatch{x,y,z}` the equivalent `vkCmdDispatch(x,y,z)` would emit. A bad handle / missing
// INDIRECT usage / out-of-range span is always a truthful error, never a false success.

/// Read `len` bytes from `buffer` at `offset` out of its bound host-visible allocation. Bytes past the
/// end of the backing store (or an unbound buffer) read as zero — an unwritten indirect arg is a `0`.
fn read_buffer_bytes(dev: &Device, buffer: VkBuffer, offset: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let Some(b) = dev.buffers.get(&buffer) else {
        return out;
    };
    let Some(mem_h) = b.bound_mem else { return out };
    let Some(m) = dev.memories.get(&mem_h) else {
        return out;
    };
    let start = b.bound_offset.saturating_add(offset) as usize;
    if start >= m.data.len() {
        return out;
    }
    let n = (m.data.len() - start).min(len);
    out[..n].copy_from_slice(&m.data[start..start + n]);
    out
}

/// Little-endian `u32` at `off` in `bytes` (0 if out of range).
struct LittleEndian<'a>(&'a [u8]);

impl LittleEndian<'_> {
    fn u32(&self, off: usize) -> u32 {
        self.0
            .get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0)
    }
}

/// Validate that `buffer` is a valid INDIRECT source holding `draw_count` argument structs of
/// `struct_size` bytes at `stride`, starting at `offset`, all within the buffer.
fn validate_indirect(
    dev: &Device,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
    struct_size: u64,
) -> Result<()> {
    let b = dev
        .buffers
        .get(&buffer)
        .ok_or(GpuError::Invalid("vkCmd*Indirect: unknown VkBuffer"))?;
    if b.usage & buffer_usage::INDIRECT == 0 {
        return Err(GpuError::Invalid(
            "vkCmd*Indirect: buffer missing INDIRECT usage",
        ));
    }
    if draw_count == 0 {
        return Ok(()); // a zero-count indirect draw is a valid no-op.
    }
    // Span from `offset` through the last argument struct's end.
    let last = (draw_count as u64 - 1)
        .checked_mul(stride as u64)
        .ok_or(GpuError::OutOfBounds)?;
    match last
        .checked_add(struct_size)
        .and_then(|span| offset.checked_add(span))
    {
        Some(end) if end <= b.size => Ok(()),
        _ => Err(GpuError::OutOfBounds),
    }
}

/// `vkCmdDrawIndirect` — validate the indirect buffer (`VkDrawIndirectCommand` is 16 bytes), read each
/// `{vertexCount, instanceCount, firstVertex, firstInstance}` argument struct out of its host-visible
/// backing, and lower each to the SAME direct `Enc::Draw` (pipeline + bind groups replayed) the
/// equivalent `vkCmdDraw` would emit — so an indirect draw and its direct twin rasterize identically.
/// Truthful error on a bad buffer.
pub fn cmd_draw_indirect(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
) -> Result<()> {
    validate_indirect(dev, buffer, offset, draw_count, stride, 16)?;
    // Snapshot every argument struct up front (immutable dev borrow) before recording.
    let args: Vec<[u32; 4]> = (0..draw_count)
        .map(|i| {
            let base = offset.saturating_add(i as u64 * stride as u64);
            let b = read_buffer_bytes(dev, buffer, base, 16);
            [
                LittleEndian(&b).u32(0),
                LittleEndian(&b).u32(4),
                LittleEndian(&b).u32(8),
                LittleEndian(&b).u32(12),
            ]
        })
        .collect();
    let rec = dev.require_recording(cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    for [vertex_count, instance_count, first_vertex, first_instance] in args {
        if let Some(p) = pipeline {
            rec.enc.push(Enc::SetPipeline(p));
        }
        for (index, group) in &groups {
            rec.enc.push(Enc::SetBindGroup {
                index: *index,
                group: *group,
            });
        }
        rec.enc.push(Enc::Draw {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        });
        rec.accumulate_occlusion(instance_count);
    }
    Ok(())
}

/// `vkCmdDrawIndexedIndirect` — validate the indirect buffer (`VkDrawIndexedIndirectCommand` is 20
/// bytes), read each `{indexCount, instanceCount, firstIndex, vertexOffset, firstInstance}` out of its
/// host-visible backing, and lower each to the SAME direct `Enc::DrawIndexed` (against the bound index
/// buffer) the equivalent `vkCmdDrawIndexed` would emit. Truthful error on a bad buffer.
pub fn cmd_draw_indexed_indirect(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
) -> Result<()> {
    validate_indirect(dev, buffer, offset, draw_count, stride, 20)?;
    let args: Vec<(u32, u32, u32, i32, u32)> = (0..draw_count)
        .map(|i| {
            let base = offset.saturating_add(i as u64 * stride as u64);
            let b = read_buffer_bytes(dev, buffer, base, 20);
            (
                LittleEndian(&b).u32(0),
                LittleEndian(&b).u32(4),
                LittleEndian(&b).u32(8),
                LittleEndian(&b).u32(12) as i32,
                LittleEndian(&b).u32(16),
            )
        })
        .collect();
    let rec = dev.require_recording(cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    for (index_count, instance_count, first_index, base_vertex, first_instance) in args {
        if let Some(p) = pipeline {
            rec.enc.push(Enc::SetPipeline(p));
        }
        for (index, group) in &groups {
            rec.enc.push(Enc::SetBindGroup {
                index: *index,
                group: *group,
            });
        }
        rec.enc.push(Enc::DrawIndexed {
            index_count,
            instance_count,
            first_index,
            base_vertex,
            first_instance,
        });
        rec.accumulate_occlusion(instance_count);
    }
    Ok(())
}

/// Read the draw count for an indirect-COUNT draw out of `count_buffer` at `count_offset` (a `u32` in the
/// buffer's host-visible backing) and clamp it to `max_draw_count` — the spec rule
/// `actual = min(countBuffer.value, maxDrawCount)`. Like the argument buffer, the count buffer is
/// host-visible unified memory the shim already holds; an unbacked/unwritten count buffer reads as `0`
/// (zero draws — a valid no-op). The count buffer must exist and carry INDIRECT usage (truthful error).
fn read_indirect_count(
    dev: &Device,
    count_buffer: VkBuffer,
    count_offset: u64,
    max_draw_count: u32,
) -> Result<u32> {
    let b = dev.buffers.get(&count_buffer).ok_or(GpuError::Invalid(
        "vkCmdDraw*IndirectCount: unknown count VkBuffer",
    ))?;
    if b.usage & buffer_usage::INDIRECT == 0 {
        return Err(GpuError::Invalid(
            "vkCmdDraw*IndirectCount: count buffer missing INDIRECT usage",
        ));
    }
    let bytes = read_buffer_bytes(dev, count_buffer, count_offset, 4);
    Ok(LittleEndian(&bytes).u32(0).min(max_draw_count))
}

/// `vkCmdDrawIndirectCount` (+ KHR/AMD aliases) — read the actual draw count from `count_buffer` (clamped
/// to `max_draw_count`, spec §20.4), then lower exactly like [`cmd_draw_indirect`] does for that many
/// `VkDrawIndirectCommand` structs: each argument struct is read out of the host-visible argument buffer
/// and lowered to the SAME direct `Enc::Draw`. Previously a recorded no-op (blank output); the count is
/// snapshotted at RECORD time out of the mapped count buffer (the common case), same honest limit as the
/// non-count indirect path. Truthful error on a bad count/argument buffer.
// The arguments intentionally mirror vkCmdDrawIndirectCount exactly at the recording boundary.
#[allow(clippy::too_many_arguments)]
pub fn cmd_draw_indirect_count(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    count_buffer: VkBuffer,
    count_offset: u64,
    max_draw_count: u32,
    stride: u32,
) -> Result<()> {
    let count = read_indirect_count(dev, count_buffer, count_offset, max_draw_count)?;
    cmd_draw_indirect(dev, cb, buffer, offset, count, stride)
}

/// `vkCmdDrawIndexedIndirectCount` (+ KHR/AMD aliases) — the indexed twin of [`cmd_draw_indirect_count`]:
/// read the actual draw count from `count_buffer` (clamped to `max_draw_count`) and lower that many
/// `VkDrawIndexedIndirectCommand` structs like [`cmd_draw_indexed_indirect`]. Truthful error on a bad
/// count/argument buffer.
// The arguments intentionally mirror vkCmdDrawIndexedIndirectCount exactly at the recording boundary.
#[allow(clippy::too_many_arguments)]
pub fn cmd_draw_indexed_indirect_count(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    count_buffer: VkBuffer,
    count_offset: u64,
    max_draw_count: u32,
    stride: u32,
) -> Result<()> {
    let count = read_indirect_count(dev, count_buffer, count_offset, max_draw_count)?;
    cmd_draw_indexed_indirect(dev, cb, buffer, offset, count, stride)
}

/// `vkCmdDispatchIndirect` — validate the indirect buffer (`VkDispatchIndirectCommand` is 12 bytes), read
/// its `{x, y, z}` workgroup counts out of the host-visible backing, and lower to the SAME
/// `BeginComputePass → SetPipeline → SetBindGroup* → Dispatch{x,y,z} → EndComputePass` sequence the
/// equivalent `vkCmdDispatch(x,y,z)` would emit (pipeline + bind groups replayed) — so an indirect
/// dispatch and its direct twin run byte-identically. Like the indirect-DRAW path the counts are
/// snapshotted at RECORD time out of the mapped/HOST_COHERENT `VkDispatchIndirectCommand` (the common
/// case); an unbacked buffer reads as zeros → a zero-count no-op dispatch. Truthful error on a bad buffer.
pub fn cmd_dispatch_indirect(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
) -> Result<()> {
    validate_indirect(dev, buffer, offset, 1, 0, 12)?;
    let b = read_buffer_bytes(dev, buffer, offset, 12);
    let (x, y, z) = (
        LittleEndian(&b).u32(0),
        LittleEndian(&b).u32(4),
        LittleEndian(&b).u32(8),
    );
    let rec = dev.require_recording(cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    rec.enc.push(Enc::BeginComputePass);
    if let Some(p) = pipeline {
        rec.enc.push(Enc::SetPipeline(p));
    }
    for (index, group) in groups {
        rec.enc.push(Enc::SetBindGroup { index, group });
    }
    rec.enc.push(Enc::Dispatch { x, y, z });
    rec.enc.push(Enc::EndComputePass);
    Ok(())
}
