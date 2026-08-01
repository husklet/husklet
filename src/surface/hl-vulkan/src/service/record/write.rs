use super::*;

/// `vkCmdClearAttachments` (one color rect) — clear a rectangle of the active render pass's color
/// target, lowered as a PASS BOUNDARY: end the pass, fill the rectangle, and reopen the pass loading
/// what was already drawn.
///
/// It cannot be a `ClearRect` recorded inside the pass. The executor refuses any transfer-shaped op
/// between `BeginRenderPass` and `EndRenderPass` — it used to drop them silently, which is worse — so an
/// op emitted there would fail the whole submit. WebGPU has no scissored clear inside a pass either, so
/// segmenting is the only lowering that performs the write, and it is the same shape the GL driver uses
/// for a scissored `glClear`.
///
/// The reopened pass LOADS every attachment. Reopening with the original load operations would re-run a
/// `LoadOp::Clear` and erase everything drawn before the clear.
pub fn cmd_clear_attachment_rect(
    dev: &mut Device,
    cb: VkCommandBuffer,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [f32; 4],
) -> Result<()> {
    if w == 0 || h == 0 {
        return Ok(()); // an empty rect clears nothing (spec-valid no-op).
    }
    let rec = dev.require_recording(cb)?;
    let texture = rec.active_render_texture.ok_or(GpuError::Invalid(
        "vkCmdClearAttachments: not inside a render pass",
    ))?;
    let (color_targets, depth_target) = rec.active_pass.clone().ok_or(GpuError::Invalid(
        "vkCmdClearAttachments: not inside a render pass",
    ))?;
    rec.enc.push(Enc::EndRenderPass);
    rec.enc.push(Enc::ClearRect {
        // A mid-pass attachment clear addresses the render target the pass is bound to, which is already
        // a single resolved subresource — hence the base level and one layer.
        base_array_layer: 0,
        layer_count: 1,
        mip_level: 0,
        texture,
        x,
        y,
        w,
        h,
        color,
    });
    let reopened: Vec<ColorAttachment> = color_targets
        .iter()
        .map(|attachment| ColorAttachment {
            load: LoadOp::Load,
            ..attachment.clone()
        })
        .collect();
    let depth_reopened = depth_target.as_ref().map(|d| DepthAttachment {
        load: LoadOp::Load,
        ..d.clone()
    });
    rec.enc.push(Enc::BeginRenderPass {
        color: reopened.clone(),
        depth: depth_reopened.clone(),
    });
    // The pass stays open from the caller's point of view, but its attachments now LOAD — a second clear
    // in the same pass must not resurrect the original clear operations either.
    rec.active_pass = Some((reopened, depth_reopened));
    Ok(())
}

// ---- buffer fills / updates (flushed as WriteBuffer at submit) ----------------------------------

/// `vkCmdFillBuffer` — fill `[dst_offset, dst_offset+size)` of `dst` with the 32-bit `data` value,
/// flushed as a `Cmd::WriteBuffer` at the start of the owning submit. `size == u64::MAX` = `VK_WHOLE_SIZE`
/// (fill to the end). The buffer must be `COPY_DST`. Ported from `command.rs::vkCmdFillBuffer`.
pub fn cmd_fill_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    dst: VkBuffer,
    dst_offset: u64,
    size: u64,
    data: u32,
) -> Result<()> {
    if !dst_offset.is_multiple_of(4) {
        return Err(GpuError::Invalid(
            "vkCmdFillBuffer: dstOffset must be 4-byte aligned",
        ));
    }
    let (ir, bsize, usage) = {
        let b = dev
            .buffers
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdFillBuffer: unknown VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    if usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdFillBuffer: buffer missing COPY_DST usage",
        ));
    }
    let fill_size = if size == u64::MAX {
        bsize.saturating_sub(dst_offset)
    } else {
        size
    };
    if fill_size == 0 || fill_size % 4 != 0 {
        return Err(GpuError::Invalid(
            "vkCmdFillBuffer: size must be a nonzero multiple of 4",
        ));
    }
    match dst_offset.checked_add(fill_size) {
        Some(end) if end <= bsize => {}
        _ => return Err(GpuError::OutOfBounds),
    }
    let words = (fill_size / 4) as usize;
    let le = data.to_le_bytes();
    let mut bytes = Vec::with_capacity(words * 4);
    for _ in 0..words {
        bytes.extend_from_slice(&le);
    }
    dev.require_recording(cb)?
        .buffer_writes
        .push((ir, dst_offset, bytes));
    Ok(())
}

/// `vkCmdUpdateBuffer` — inline-update `[dst_offset, dst_offset+len)` of `dst` from `data`, flushed as a
/// `Cmd::WriteBuffer` at the start of the owning submit. `data` ≤ 65536 bytes, a multiple of 4;
/// `dst_offset` 4-byte aligned. The buffer must be `COPY_DST`. Ported from `command.rs::vkCmdUpdateBuffer`.
pub fn cmd_update_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    dst: VkBuffer,
    dst_offset: u64,
    data: &[u8],
) -> Result<()> {
    if data.is_empty()
        || data.len() > 65536
        || !data.len().is_multiple_of(4)
        || !dst_offset.is_multiple_of(4)
    {
        return Err(GpuError::Invalid(
            "vkCmdUpdateBuffer: bad dataSize/dstOffset (≤64KiB, 4-byte multiple)",
        ));
    }
    let (ir, bsize, usage) = {
        let b = dev
            .buffers
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdUpdateBuffer: unknown VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    if usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdUpdateBuffer: buffer missing COPY_DST usage",
        ));
    }
    match dst_offset.checked_add(data.len() as u64) {
        Some(end) if end <= bsize => {}
        _ => return Err(GpuError::OutOfBounds),
    }
    dev.require_recording(cb)?
        .buffer_writes
        .push((ir, dst_offset, data.to_vec()));
    Ok(())
}
