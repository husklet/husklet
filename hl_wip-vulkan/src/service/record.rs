//! Command-buffer recording — the `vkCmd*` → [`Enc`] lowering, plus the descriptor-set → bind-group
//! lowering `vkCmdBindDescriptorSets` performs.
//!
//! Ported from `hl-shim-vk/src/command.rs`. Each `vkCmd*` appends the encoder op(s) it lowers to onto
//! the target command buffer's recording; `vkQueueSubmit` ([`super::submit`]) ships the recorded
//! encoder as one [`hl_gpu::Cmd::Submit`]. The one command that emits a resource-level `Cmd` while
//! recording is `vkCmdBindDescriptorSets`: it resolves each set's `binding -> buffer` table into a
//! [`Cmd::CreateBindGroup`] (dynamic offsets applied here) and remembers the `(set, bind-group)` pair
//! to replay into the next pass.

use crate::model::command::{CmdBufRec, CommandBufferState};
use crate::model::sync::DeferredOp;
use crate::*;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, ColorAttachment, DepthAttachment, Extent3d, Origin3d,
    TextureSubresource,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, Filter, IndexFormat, LoadOp};
use hl_gpu::{Cmd, CommandSink, GpuError, Result};
use std::collections::HashMap;

/// `vkAllocateCommandBuffers` (one buffer) — mint an `Initial` command buffer.
pub fn allocate_command_buffer(dev: &mut Device) -> VkCommandBuffer {
    let handle = dev.alloc_handle();
    dev.command_buffers.insert(handle, CmdBufRec::initial());
    handle
}

/// `vkBeginCommandBuffer` — move the buffer to `Recording` and clear any prior recording.
pub fn begin(dev: &mut Device, cb: VkCommandBuffer) -> Result<()> {
    let rec = dev
        .command_buffers
        .get_mut(&cb)
        .ok_or(GpuError::Invalid("vkBeginCommandBuffer: unknown VkCommandBuffer"))?;
    rec.reset_recording();
    rec.state = CommandBufferState::Recording;
    Ok(())
}

/// `vkEndCommandBuffer` — move the buffer to `Executable` (submittable).
pub fn end(dev: &mut Device, cb: VkCommandBuffer) -> Result<()> {
    let rec = dev
        .command_buffers
        .get_mut(&cb)
        .ok_or(GpuError::Invalid("vkEndCommandBuffer: unknown VkCommandBuffer"))?;
    if rec.state != CommandBufferState::Recording {
        return Err(GpuError::Invalid("vkEndCommandBuffer: buffer is not recording"));
    }
    rec.state = CommandBufferState::Executable;
    Ok(())
}

/// Borrow a command buffer ONLY if it is `Recording` (the Vulkan rule that a `vkCmd*` outside an active
/// begin/end is invalid). Ported from `VkState::recording_mut`.
fn recording_mut<'a>(dev: &'a mut Device, cb: VkCommandBuffer) -> Result<&'a mut CmdBufRec> {
    match dev.command_buffers.get_mut(&cb) {
        Some(r) if r.state == CommandBufferState::Recording => Ok(r),
        _ => Err(GpuError::Invalid("vkCmd*: command buffer is not recording")),
    }
}

/// `vkCmdBindPipeline` — remember the bound hl-GPU pipeline id + kind for the next pass.
pub fn cmd_bind_pipeline(dev: &mut Device, cb: VkCommandBuffer, pipeline: VkPipeline) -> Result<()> {
    let (ir, kind) = {
        let p = dev
            .pipelines
            .get(&pipeline)
            .ok_or(GpuError::Invalid("vkCmdBindPipeline: unknown VkPipeline"))?;
        (p.ir_id, p.kind)
    };
    let rec = recording_mut(dev, cb)?;
    rec.bound_pipeline = Some(ir);
    rec.bound_pipeline_kind = Some(kind);
    Ok(())
}

/// `vkCmdBindDescriptorSets` — resolve each set's `binding -> (buffer, offset, range)` table into a
/// [`Cmd::CreateBindGroup`] (applying that set's `pDynamicOffsets`) and record the `(set, bind-group)`
/// pair to replay into the next pass. Ported from `command.rs::vkCmdBindDescriptorSets`.
pub fn cmd_bind_descriptor_sets(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    cb: VkCommandBuffer,
    first_set: u32,
    sets: &[VkDescriptorSet],
    dynamic_offsets: &[u32],
) -> Result<()> {
    let mut dyn_cursor = 0usize; // global cursor across all bound sets
    for (i, &dset) in sets.iter().enumerate() {
        let set_index = first_set + i as u32;
        // Snapshot the set's (binding -> buffer) table + its layout handle (owned; borrows end here).
        let Some(rec) = dev.descriptor_sets.get(&dset) else { continue };
        let layout_handle = rec.layout;
        let mut pairs: Vec<(u32, (VkBuffer, u64, u64))> =
            rec.buffers.iter().map(|(b, v)| (*b, *v)).collect();
        pairs.sort_by_key(|(b, _)| *b);
        // Consume this set's dynamic offsets (its layout's dynamic-buffer bindings, ascending).
        let dyn_bindings = dev
            .set_layouts
            .get(&layout_handle)
            .map(|l| l.dynamic_bindings())
            .unwrap_or_default();
        let mut extra: HashMap<u32, u64> = HashMap::new();
        for db in dyn_bindings {
            if dyn_cursor < dynamic_offsets.len() {
                extra.insert(db, dynamic_offsets[dyn_cursor] as u64);
                dyn_cursor += 1;
            }
        }
        // Resolve each binding's buffer handle to its hl-GPU id, applying the dynamic offset.
        let mut entries: Vec<BindEntry> = Vec::new();
        for (binding, (buf_handle, offset, size)) in pairs {
            if let Some(b) = dev.buffers.get(&buf_handle) {
                entries.push(BindEntry {
                    binding,
                    resource: BindResource::Buffer {
                        id: b.ir_id,
                        offset: offset + extra.get(&binding).copied().unwrap_or(0),
                        size,
                    },
                });
            }
        }
        let ir_id = dev.alloc_ir();
        sink.submit(&[Cmd::CreateBindGroup(ir_id, BindGroupDesc { set: set_index, entries })])?;
        if let Ok(cbrec) = recording_mut(dev, cb) {
            cbrec.pending_bind_groups.push((set_index, ir_id));
        }
    }
    Ok(())
}

/// `vkCmdDispatch` — record a compute pass: `BeginComputePass` → `SetPipeline` → `SetBindGroup`* →
/// `Dispatch` → `EndComputePass`. Ported from `command.rs::vkCmdDispatch`.
pub fn cmd_dispatch(dev: &mut Device, cb: VkCommandBuffer, x: u32, y: u32, z: u32) -> Result<()> {
    let rec = recording_mut(dev, cb)?;
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

/// `vkCmdBeginRenderPass` — begin a render pass targeting `color_image` with one color attachment
/// (`Clear` when `load_clear`, else `Load`; always stored). Ported from `command.rs::vkCmdBeginRenderPass`.
pub fn cmd_begin_render_pass(
    dev: &mut Device,
    cb: VkCommandBuffer,
    color_image: VkImage,
    clear: [f32; 4],
    load_clear: bool,
) -> Result<()> {
    let texture = dev
        .images
        .get(&color_image)
        .ok_or(GpuError::Invalid("vkCmdBeginRenderPass: unknown color VkImage"))?
        .ir_id;
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture,
            load: if load_clear { LoadOp::Clear } else { LoadOp::Load },
            clear,
            store: true,
        }],
        depth: None,
    });
    rec.in_render_pass = true;
    rec.active_render_texture = Some(texture);
    Ok(())
}

/// `vkCmdBindVertexBuffers` (one binding) — record `SetVertexBuffer`.
pub fn cmd_bind_vertex_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    slot: u32,
    buffer: VkBuffer,
    offset: u64,
) -> Result<()> {
    let ir = dev
        .buffers
        .get(&buffer)
        .ok_or(GpuError::Invalid("vkCmdBindVertexBuffers: unknown VkBuffer"))?
        .ir_id;
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::SetVertexBuffer { slot, buffer: ir, offset });
    Ok(())
}

/// `vkCmdBindIndexBuffer` — record `SetIndexBuffer` for the bound index buffer. `vk_index_type` is a raw
/// `VkIndexType` (`VK_INDEX_TYPE_UINT16` = 0, `VK_INDEX_TYPE_UINT32` = 1).
pub fn cmd_bind_index_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    vk_index_type: u32,
) -> Result<()> {
    let ir = dev
        .buffers
        .get(&buffer)
        .ok_or(GpuError::Invalid("vkCmdBindIndexBuffer: unknown VkBuffer"))?
        .ir_id;
    let format = if vk_index_type == 1 { IndexFormat::U32 } else { IndexFormat::U16 };
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::SetIndexBuffer { buffer: ir, offset, format });
    Ok(())
}

/// `vkCmdDraw` — replay the bound pipeline + bind groups, then record `Draw`. Ported from
/// `command.rs::vkCmdDraw`.
pub fn cmd_draw(
    dev: &mut Device,
    cb: VkCommandBuffer,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) -> Result<()> {
    let rec = recording_mut(dev, cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    if let Some(p) = pipeline {
        rec.enc.push(Enc::SetPipeline(p));
    }
    for (index, group) in groups {
        rec.enc.push(Enc::SetBindGroup { index, group });
    }
    rec.enc.push(Enc::Draw { vertex_count, instance_count, first_vertex, first_instance });
    Ok(())
}

/// `vkCmdDrawIndexed` — replay the bound pipeline + bind groups, then record `DrawIndexed` against the
/// bound index buffer. Ported from `command.rs::vkCmdDrawIndexed`.
pub fn cmd_draw_indexed(
    dev: &mut Device,
    cb: VkCommandBuffer,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) -> Result<()> {
    let rec = recording_mut(dev, cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    if let Some(p) = pipeline {
        rec.enc.push(Enc::SetPipeline(p));
    }
    for (index, group) in groups {
        rec.enc.push(Enc::SetBindGroup { index, group });
    }
    rec.enc.push(Enc::DrawIndexed {
        index_count,
        instance_count,
        first_index,
        base_vertex: vertex_offset,
        first_instance,
    });
    Ok(())
}

// ---- dynamic rendering (VK_KHR_dynamic_rendering / core 1.3) ------------------------------------
// A dynamic-rendering pass carries its attachments inline in `VkRenderingInfo` — no `VkRenderPass` or
// `VkFramebuffer` object. `vkCmdBeginRendering` lowers to the SAME `Enc::BeginRenderPass` a
// `vkCmdBeginRenderPass` does; the only difference is where the color/depth targets come from (the
// inline `pColorAttachments`/`pDepthAttachment`, resolved to image ir ids by the shim). `vkCmdEndRendering`
// is identical to `vkCmdEndRenderPass` (`Enc::EndRenderPass`), so it reuses [`cmd_end_render_pass`].

/// One parsed `VkRenderingAttachmentInfo` color target: the color `VkImage` (resolved from the
/// attachment's `imageView` by the shim), its clear value, and whether its `loadOp`/`storeOp` are
/// CLEAR/STORE. Neutral (no C ABI) so the lowering is unit-testable against a `RecordingSink`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderingColorAttachment {
    /// The color `VkImage` handle the attachment's `imageView` resolves to.
    pub image: VkImage,
    /// The RGBA clear value (`VkRenderingAttachmentInfo::clearValue.color`).
    pub clear: [f32; 4],
    /// `loadOp == VK_ATTACHMENT_LOAD_OP_CLEAR` (else the existing contents are loaded).
    pub load_clear: bool,
    /// `storeOp == VK_ATTACHMENT_STORE_OP_STORE` (else the result may be discarded).
    pub store: bool,
}

/// One parsed `VkRenderingAttachmentInfo` depth target.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderingDepthAttachment {
    /// The depth `VkImage` handle the attachment's `imageView` resolves to.
    pub image: VkImage,
    /// The depth clear value (`VkRenderingAttachmentInfo::clearValue.depthStencil.depth`).
    pub clear_depth: f32,
    /// `loadOp == VK_ATTACHMENT_LOAD_OP_CLEAR`.
    pub load_clear: bool,
}

/// `vkCmdBeginRendering(KHR)` — begin a render-pass-object-free pass from `VkRenderingInfo`: each color
/// attachment lowers to a [`ColorAttachment`] (`Clear` when its `loadOp` is CLEAR, else `Load`) and an
/// optional depth attachment to a [`DepthAttachment`], emitted as one [`Enc::BeginRenderPass`] — the SAME
/// op a classic render pass lowers to (§ dynamic rendering). Every attachment image must exist. The active
/// clear target (`vkCmdClearAttachments`) is the first color attachment.
pub fn cmd_begin_rendering(
    dev: &mut Device,
    cb: VkCommandBuffer,
    colors: &[RenderingColorAttachment],
    depth: Option<RenderingDepthAttachment>,
) -> Result<()> {
    // Resolve every attachment image → ir texture id up front (a bad handle fails before recording).
    let mut color_targets: Vec<ColorAttachment> = Vec::with_capacity(colors.len());
    for c in colors {
        let texture = dev
            .images
            .get(&c.image)
            .ok_or(GpuError::Invalid("vkCmdBeginRendering: unknown color VkImage"))?
            .ir_id;
        color_targets.push(ColorAttachment {
            texture,
            load: if c.load_clear { LoadOp::Clear } else { LoadOp::Load },
            clear: c.clear,
            store: c.store,
        });
    }
    let depth_target = match depth {
        Some(d) => {
            let texture = dev
                .images
                .get(&d.image)
                .ok_or(GpuError::Invalid("vkCmdBeginRendering: unknown depth VkImage"))?
                .ir_id;
            Some(DepthAttachment {
                texture,
                load: if d.load_clear { LoadOp::Clear } else { LoadOp::Load },
                clear_depth: d.clear_depth,
            })
        }
        None => None,
    };
    let active = color_targets.first().map(|c| c.texture);
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::BeginRenderPass { color: color_targets, depth: depth_target });
    rec.in_render_pass = true;
    rec.active_render_texture = active;
    Ok(())
}

/// `vkCmdEndRenderPass` — close the render pass.
pub fn cmd_end_render_pass(dev: &mut Device, cb: VkCommandBuffer) -> Result<()> {
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::EndRenderPass);
    rec.in_render_pass = false;
    rec.active_render_texture = None;
    Ok(())
}

// ---- secondary command buffers -----------------------------------------------------------------

/// `vkCmdExecuteCommands` — replay recorded secondary command buffers into this primary: each
/// secondary's encoder ops, its deferred device query/event ops, and its inline buffer writes are
/// appended to the primary in order (so a later `vkQueueSubmit` of the primary ships the spliced work as
/// one stream). The primary must be `Recording`; every secondary must exist and be `Executable`
/// (validated up front so a bad batch splices nothing). Ported from
/// `hl-shim-vk/src/command.rs::vkCmdExecuteCommands` (`MVKCmdExecuteCommands`).
pub fn cmd_execute_commands(
    dev: &mut Device,
    primary: VkCommandBuffer,
    secondaries: &[VkCommandBuffer],
) -> Result<()> {
    // The primary must be recording.
    let _ = recording_mut(dev, primary)?;
    // Every secondary must exist and be Executable — validate before splicing anything.
    for &sec in secondaries {
        match dev.command_buffers.get(&sec) {
            Some(r) if r.state == CommandBufferState::Executable => {}
            _ => return Err(GpuError::Invalid("vkCmdExecuteCommands: secondary is unknown or not executable")),
        }
    }
    // Snapshot each secondary's recorded work (immutable borrows end), then splice in order.
    let mut spliced: Vec<(Vec<Enc>, Vec<(u32, u64, Vec<u8>)>, Vec<DeferredOp>)> = Vec::new();
    for &sec in secondaries {
        if let Some(r) = dev.command_buffers.get(&sec) {
            spliced.push((r.enc.clone(), r.buffer_writes.clone(), r.deferred.clone()));
        }
    }
    let primary_rec = recording_mut(dev, primary)?;
    for (enc, writes, deferred) in spliced {
        primary_rec.enc.extend(enc);
        primary_rec.buffer_writes.extend(writes);
        primary_rec.deferred.extend(deferred);
    }
    Ok(())
}

// ---- dynamic state -----------------------------------------------------------------------------
// Viewport + scissor are modeled by the IR (etag 7 / 16) and lower to real encoder ops. The remaining
// `vkCmdSet*` dynamic state is recorded into the command buffer's [`DynamicState`] (observable, honest)
// but carries no encoder op — the software color rasterizer does not model wide lines, depth bias,
// constant-color blend, or stencil. Ported from `command.rs`'s state-setting commands.

/// `vkCmdSetViewport` (one viewport) — record `Enc::SetViewport`. The viewport transform is applied by
/// the pass/rasterizer that consumes it.
#[allow(clippy::too_many_arguments)]
pub fn cmd_set_viewport(
    dev: &mut Device,
    cb: VkCommandBuffer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    min_depth: f32,
    max_depth: f32,
) -> Result<()> {
    recording_mut(dev, cb)?.enc.push(Enc::SetViewport { x, y, w, h, min_depth, max_depth });
    Ok(())
}

/// `vkCmdSetScissor` (one rect) — record `Enc::SetScissor`. A negative `VkRect2D` offset is clamped to 0
/// by the caller (the IR scissor is unsigned).
pub fn cmd_set_scissor(dev: &mut Device, cb: VkCommandBuffer, x: u32, y: u32, w: u32, h: u32) -> Result<()> {
    recording_mut(dev, cb)?.enc.push(Enc::SetScissor { x, y, w, h });
    Ok(())
}

/// `vkCmdSetLineWidth` — record the dynamic line width (honest command state; the fill rasterizer draws
/// no wide lines, so this emits no encoder op — documented in [`DynamicState`]).
pub fn cmd_set_line_width(dev: &mut Device, cb: VkCommandBuffer, line_width: f32) -> Result<()> {
    recording_mut(dev, cb)?.dynamic.line_width = line_width;
    Ok(())
}

/// `vkCmdSetDepthBias` — record `(constantFactor, clamp, slopeFactor)` (no depth buffer; no encoder op).
pub fn cmd_set_depth_bias(
    dev: &mut Device,
    cb: VkCommandBuffer,
    constant_factor: f32,
    clamp: f32,
    slope_factor: f32,
) -> Result<()> {
    recording_mut(dev, cb)?.dynamic.depth_bias = (constant_factor, clamp, slope_factor);
    Ok(())
}

/// `vkCmdSetBlendConstants` — record the RGBA blend constants (no constant-color blend; no encoder op).
pub fn cmd_set_blend_constants(dev: &mut Device, cb: VkCommandBuffer, constants: [f32; 4]) -> Result<()> {
    recording_mut(dev, cb)?.dynamic.blend_constants = constants;
    Ok(())
}

/// Apply `value` to the stencil-face pair selected by `face_mask` (VkStencilFaceFlags: FRONT = 0x1,
/// BACK = 0x2, FRONT_AND_BACK = 0x3).
fn set_stencil_faces(pair: &mut (u32, u32), face_mask: u32, value: u32) {
    if face_mask & 0x1 != 0 {
        pair.0 = value;
    }
    if face_mask & 0x2 != 0 {
        pair.1 = value;
    }
}

/// `vkCmdSetStencilCompareMask` — record the compare mask for the selected face(s) (no stencil buffer).
pub fn cmd_set_stencil_compare_mask(dev: &mut Device, cb: VkCommandBuffer, face_mask: u32, mask: u32) -> Result<()> {
    set_stencil_faces(&mut recording_mut(dev, cb)?.dynamic.stencil_compare_mask, face_mask, mask);
    Ok(())
}

/// `vkCmdSetStencilWriteMask` — record the write mask for the selected face(s) (no stencil buffer).
pub fn cmd_set_stencil_write_mask(dev: &mut Device, cb: VkCommandBuffer, face_mask: u32, mask: u32) -> Result<()> {
    set_stencil_faces(&mut recording_mut(dev, cb)?.dynamic.stencil_write_mask, face_mask, mask);
    Ok(())
}

/// `vkCmdSetStencilReference` — record the reference value for the selected face(s) (no stencil buffer).
pub fn cmd_set_stencil_reference(dev: &mut Device, cb: VkCommandBuffer, face_mask: u32, reference: u32) -> Result<()> {
    set_stencil_faces(&mut recording_mut(dev, cb)?.dynamic.stencil_reference, face_mask, reference);
    Ok(())
}

// ---- push constants ----------------------------------------------------------------------------

/// `vkCmdPushConstants` — write `bytes` at `offset` into the command buffer's push-constant block. The
/// bytes are retained as honest command state (grown on demand): the hl-GPU IR has no push-constant
/// channel yet, so a draw/dispatch cannot bind them, but they are never silently dropped (a later
/// increment stages them as a per-draw uniform bind). `offset`/size must be 4-byte aligned + nonzero
/// (spec §17.1). Ported from `command.rs::vkCmdPushConstants` (range-vs-layout validation omitted — the
/// bring-up pipeline-layout model does not carry push-constant ranges).
pub fn cmd_push_constants(dev: &mut Device, cb: VkCommandBuffer, offset: u32, bytes: &[u8]) -> Result<()> {
    if offset % 4 != 0 || bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(GpuError::Invalid("vkCmdPushConstants: offset/size must be nonzero and 4-byte aligned"));
    }
    let rec = recording_mut(dev, cb)?;
    let end = offset as usize + bytes.len();
    if rec.push_constants.len() < end {
        rec.push_constants.resize(end, 0);
    }
    rec.push_constants[offset as usize..end].copy_from_slice(bytes);
    Ok(())
}

// ---- indirect draws / dispatch -----------------------------------------------------------------
// The indirect commands read their draw/dispatch arguments from a device buffer at execution time. The
// hl-GPU IR carries only direct `Draw`/`Dispatch`/`DrawIndexed` (the argument words are not available at
// record time and there is no indirect encoder op), so these validate the indirect buffer (handle,
// INDIRECT usage, in-bounds argument span) and record NO encoder op — an honest, documented limit. A bad
// handle / missing usage / out-of-range span is a truthful error, never a false success. Ported from
// `command.rs::vkCmdDrawIndirect`/`indirect_ok`.

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
        return Err(GpuError::Invalid("vkCmd*Indirect: buffer missing INDIRECT usage"));
    }
    if draw_count == 0 {
        return Ok(()); // a zero-count indirect draw is a valid no-op.
    }
    // Span from `offset` through the last argument struct's end.
    let last = (draw_count as u64 - 1).checked_mul(stride as u64).ok_or(GpuError::OutOfBounds)?;
    match last.checked_add(struct_size).and_then(|span| offset.checked_add(span)) {
        Some(end) if end <= b.size => Ok(()),
        _ => Err(GpuError::OutOfBounds),
    }
}

/// `vkCmdDrawIndirect` — validate the indirect buffer (`VkDrawIndirectCommand` is 16 bytes) then record
/// no encoder op (documented limit: the IR has no indirect draw). Truthful error on a bad buffer.
pub fn cmd_draw_indirect(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
) -> Result<()> {
    validate_indirect(dev, buffer, offset, draw_count, stride, 16)?;
    let _ = recording_mut(dev, cb)?;
    Ok(())
}

/// `vkCmdDrawIndexedIndirect` — validate the indirect buffer (`VkDrawIndexedIndirectCommand` is 20 bytes)
/// then record no encoder op (documented limit). Truthful error on a bad buffer.
pub fn cmd_draw_indexed_indirect(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
) -> Result<()> {
    validate_indirect(dev, buffer, offset, draw_count, stride, 20)?;
    let _ = recording_mut(dev, cb)?;
    Ok(())
}

/// `vkCmdDispatchIndirect` — validate the indirect buffer (`VkDispatchIndirectCommand` is 12 bytes) then
/// record no encoder op (documented limit). Truthful error on a bad buffer.
pub fn cmd_dispatch_indirect(dev: &mut Device, cb: VkCommandBuffer, buffer: VkBuffer, offset: u64) -> Result<()> {
    validate_indirect(dev, buffer, offset, 1, 0, 12)?;
    let _ = recording_mut(dev, cb)?;
    Ok(())
}

/// `vkCmdCopyBuffer` (one region) — record a `CopyBufferToBuffer`. Ported from
/// `command.rs::vkCmdCopyBuffer`.
pub fn cmd_copy_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkBuffer,
    dst: VkBuffer,
    src_offset: u64,
    dst_offset: u64,
    size: u64,
) -> Result<()> {
    let src_ir = dev
        .buffers
        .get(&src)
        .ok_or(GpuError::Invalid("vkCmdCopyBuffer: unknown src VkBuffer"))?
        .ir_id;
    let dst_ir = dev
        .buffers
        .get(&dst)
        .ok_or(GpuError::Invalid("vkCmdCopyBuffer: unknown dst VkBuffer"))?
        .ir_id;
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::CopyBufferToBuffer {
        src: src_ir,
        src_offset,
        dst: dst_ir,
        dst_offset,
        size,
    });
    Ok(())
}

// ---- buffer <-> image copies -------------------------------------------------------------------
// The hl model images are single-mip, single-layer 2D render/transfer targets (base subresource); the
// copies below lower one `VkBufferImageCopy` region to the matching encoder op with mip 0 / layer 0.
// Ported (simplified for that model) from `command.rs::vkCmdCopyBufferToImage` / `vkCmdCopyImageToBuffer`.

/// The `bytes_per_row` a `VkBufferImageCopy` implies (`bufferRowLength` in texels, 0 = tight-packed to
/// `width`), plus a bounds check that the described span fits in `buf_size`. RGBA8 (4 bytes/texel) — the
/// materialized color subset. `None` (rejected) on a too-narrow row, a too-short image height, or an
/// out-of-bounds span. Mirrors the reference's `checked` buffer-span math.
fn buffer_image_bytes_per_row(
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
    buf_size: u64,
) -> Option<u32> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_texels = if row_length_texels == 0 { width } else { row_length_texels };
    let image_rows = if image_height_rows == 0 { height } else { image_height_rows };
    if row_texels < width || image_rows < height {
        return None;
    }
    let bytes_per_row = row_texels.checked_mul(4)?;
    let end = (bytes_per_row as u64)
        .checked_mul(height.saturating_sub(1) as u64)?
        .checked_add(width as u64 * 4)?
        .checked_add(buffer_offset)?;
    (end <= buf_size).then_some(bytes_per_row)
}

/// `vkCmdCopyBufferToImage` (one region, base subresource) — record `CopyBufferToTexture`. The src buffer
/// must be `COPY_SRC` and the dst image `COPY_DST`; the region must fit the buffer + the image extent.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_buffer_to_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkBuffer,
    dst: VkImage,
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    let (src_ir, buf_size, buf_usage) = {
        let b = dev
            .buffers
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdCopyBufferToImage: unknown src VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    let (dst_ir, img_usage, iw, ih) = {
        let i = dev
            .images
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdCopyBufferToImage: unknown dst VkImage"))?;
        (i.ir_id, i.usage, i.width, i.height)
    };
    if buf_usage & buffer_usage::COPY_SRC == 0 || img_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdCopyBufferToImage: missing COPY_SRC/COPY_DST usage"));
    }
    if width > iw || height > ih {
        return Err(GpuError::OutOfBounds);
    }
    let bytes_per_row =
        buffer_image_bytes_per_row(buffer_offset, row_length_texels, image_height_rows, width, height, buf_size)
            .ok_or(GpuError::OutOfBounds)?;
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::CopyBufferToTexture {
        src: src_ir,
        src_offset: buffer_offset,
        bytes_per_row,
        dst: dst_ir,
        mip: 0,
        width,
        height,
    });
    Ok(())
}

/// `vkCmdCopyImageToBuffer` (one region, base subresource) — record `CopyTextureToBuffer`. The src image
/// must be `COPY_SRC` and the dst buffer `COPY_DST`; the region must fit the image extent + the buffer.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_image_to_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkBuffer,
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    let (src_ir, img_usage, iw, ih) = {
        let i = dev
            .images
            .get(&src)
            .ok_or(GpuError::Invalid("vkCmdCopyImageToBuffer: unknown src VkImage"))?;
        (i.ir_id, i.usage, i.width, i.height)
    };
    let (dst_ir, buf_size, buf_usage) = {
        let b = dev
            .buffers
            .get(&dst)
            .ok_or(GpuError::Invalid("vkCmdCopyImageToBuffer: unknown dst VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    if img_usage & texture_usage::COPY_SRC == 0 || buf_usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdCopyImageToBuffer: missing COPY_SRC/COPY_DST usage"));
    }
    if width > iw || height > ih {
        return Err(GpuError::OutOfBounds);
    }
    let bytes_per_row =
        buffer_image_bytes_per_row(buffer_offset, row_length_texels, image_height_rows, width, height, buf_size)
            .ok_or(GpuError::OutOfBounds)?;
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::CopyTextureToBuffer {
        src: src_ir,
        mip: 0,
        width,
        height,
        dst: dst_ir,
        dst_offset: buffer_offset,
        bytes_per_row,
    });
    Ok(())
}

/// `vkCmdCopyImage` (one region, base subresource) — record an exact-size `CopyTextureToTexture`. Formats
/// must match; both usages present; both regions in-bounds; overlapping same-image self-copy rejected.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_origin: (u32, u32),
    dst_origin: (u32, u32),
    extent: (u32, u32),
) -> Result<()> {
    let (src_ir, src_fmt, src_usage, siw, sih) = {
        let i = dev.images.get(&src).ok_or(GpuError::Invalid("vkCmdCopyImage: unknown src VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    let (dst_ir, dst_fmt, dst_usage, diw, dih) = {
        let i = dev.images.get(&dst).ok_or(GpuError::Invalid("vkCmdCopyImage: unknown dst VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    if src_fmt != dst_fmt {
        return Err(GpuError::Invalid("vkCmdCopyImage: source and destination formats differ"));
    }
    if src_usage & texture_usage::COPY_SRC == 0 || dst_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdCopyImage: missing COPY_SRC/COPY_DST usage"));
    }
    let (w, h) = extent;
    if w == 0 || h == 0 {
        return Err(GpuError::OutOfBounds);
    }
    if src_origin.0 + w > siw || src_origin.1 + h > sih || dst_origin.0 + w > diw || dst_origin.1 + h > dih {
        return Err(GpuError::OutOfBounds);
    }
    // A same-image overlapping self-copy is undefined; reject it (the reference does).
    if src == dst
        && src_origin.0 < dst_origin.0 + w
        && dst_origin.0 < src_origin.0 + w
        && src_origin.1 < dst_origin.1 + h
        && dst_origin.1 < src_origin.1 + h
    {
        return Err(GpuError::Invalid("vkCmdCopyImage: overlapping self-copy"));
    }
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::CopyTextureToTexture {
        src: src_ir,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d { x: src_origin.0, y: src_origin.1, z: 0 },
        dst: dst_ir,
        dst_sub: TextureSubresource::base(),
        dst_origin: Origin3d { x: dst_origin.0, y: dst_origin.1, z: 0 },
        extent: Extent3d { width: w, height: h, depth: 1 },
    });
    Ok(())
}

/// `vkCmdBlitImage` (one region, base subresource) — record a scaled/filtered `BlitTexture`. Distinct
/// images, matching formats, both usages present, positive src/dst extents in-bounds. `linear` selects
/// the resampling filter (`VK_FILTER_LINEAR` → [`Filter::Linear`], else [`Filter::Nearest`]).
#[allow(clippy::too_many_arguments)]
pub fn cmd_blit_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkImage,
    src_origin: (u32, u32),
    src_extent: (u32, u32),
    dst_origin: (u32, u32),
    dst_extent: (u32, u32),
    linear: bool,
) -> Result<()> {
    if src == dst {
        return Err(GpuError::Invalid("vkCmdBlitImage: src and dst image must differ"));
    }
    let (src_ir, src_fmt, src_usage, siw, sih) = {
        let i = dev.images.get(&src).ok_or(GpuError::Invalid("vkCmdBlitImage: unknown src VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    let (dst_ir, dst_fmt, dst_usage, diw, dih) = {
        let i = dev.images.get(&dst).ok_or(GpuError::Invalid("vkCmdBlitImage: unknown dst VkImage"))?;
        (i.ir_id, i.format, i.usage, i.width, i.height)
    };
    if src_fmt != dst_fmt {
        return Err(GpuError::Invalid("vkCmdBlitImage: source and destination formats differ"));
    }
    if src_usage & texture_usage::COPY_SRC == 0 || dst_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdBlitImage: missing COPY_SRC/COPY_DST usage"));
    }
    if src_extent.0 == 0 || src_extent.1 == 0 || dst_extent.0 == 0 || dst_extent.1 == 0 {
        return Err(GpuError::OutOfBounds);
    }
    if src_origin.0 + src_extent.0 > siw
        || src_origin.1 + src_extent.1 > sih
        || dst_origin.0 + dst_extent.0 > diw
        || dst_origin.1 + dst_extent.1 > dih
    {
        return Err(GpuError::OutOfBounds);
    }
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::BlitTexture {
        src: src_ir,
        src_sub: TextureSubresource::base(),
        src_origin: Origin3d { x: src_origin.0, y: src_origin.1, z: 0 },
        src_extent: Extent3d { width: src_extent.0, height: src_extent.1, depth: 1 },
        dst: dst_ir,
        dst_sub: TextureSubresource::base(),
        dst_origin: Origin3d { x: dst_origin.0, y: dst_origin.1, z: 0 },
        dst_extent: Extent3d { width: dst_extent.0, height: dst_extent.1, depth: 1 },
        filter: if linear { Filter::Linear } else { Filter::Nearest },
    });
    Ok(())
}

// ---- clears ------------------------------------------------------------------------------------

/// `vkCmdClearColorImage` (base subresource) — record a full-extent `ClearRect` on the image. The image
/// must be `COPY_DST` (a transfer-clear target). Ported from `command.rs::vkCmdClearColorImage`.
pub fn cmd_clear_color_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    image: VkImage,
    color: [f32; 4],
) -> Result<()> {
    let (ir, usage, w, h) = {
        let i = dev
            .images
            .get(&image)
            .ok_or(GpuError::Invalid("vkCmdClearColorImage: unknown VkImage"))?;
        (i.ir_id, i.usage, i.width, i.height)
    };
    if usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdClearColorImage: image missing COPY_DST usage"));
    }
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::ClearRect { texture: ir, x: 0, y: 0, w, h, color });
    Ok(())
}

/// `vkCmdClearAttachments` (one color rect) — record a `ClearRect` on the active render pass's color
/// target. Must be inside a render pass. Ported from `command.rs::vkCmdClearAttachments` (color subset).
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
    let rec = recording_mut(dev, cb)?;
    let texture = rec
        .active_render_texture
        .ok_or(GpuError::Invalid("vkCmdClearAttachments: not inside a render pass"))?;
    rec.enc.push(Enc::ClearRect { texture, x, y, w, h, color });
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
    if dst_offset % 4 != 0 {
        return Err(GpuError::Invalid("vkCmdFillBuffer: dstOffset must be 4-byte aligned"));
    }
    let (ir, bsize, usage) = {
        let b = dev.buffers.get(&dst).ok_or(GpuError::Invalid("vkCmdFillBuffer: unknown VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    if usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdFillBuffer: buffer missing COPY_DST usage"));
    }
    let fill_size = if size == u64::MAX { bsize.saturating_sub(dst_offset) } else { size };
    if fill_size == 0 || fill_size % 4 != 0 {
        return Err(GpuError::Invalid("vkCmdFillBuffer: size must be a nonzero multiple of 4"));
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
    recording_mut(dev, cb)?.buffer_writes.push((ir, dst_offset, bytes));
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
    if data.is_empty() || data.len() > 65536 || data.len() % 4 != 0 || dst_offset % 4 != 0 {
        return Err(GpuError::Invalid("vkCmdUpdateBuffer: bad dataSize/dstOffset (≤64KiB, 4-byte multiple)"));
    }
    let (ir, bsize, usage) = {
        let b = dev.buffers.get(&dst).ok_or(GpuError::Invalid("vkCmdUpdateBuffer: unknown VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    if usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdUpdateBuffer: buffer missing COPY_DST usage"));
    }
    match dst_offset.checked_add(data.len() as u64) {
        Some(end) if end <= bsize => {}
        _ => return Err(GpuError::OutOfBounds),
    }
    recording_mut(dev, cb)?.buffer_writes.push((ir, dst_offset, data.to_vec()));
    Ok(())
}

// ---- pipeline barriers (layout bookkeeping; layout-implicit IR emits nothing) -------------------

/// `vkCmdPipelineBarrier` / `vkCmdPipelineBarrier2` (image memory barriers) — record each image's
/// `oldLayout → newLayout` transition in the device's layout bookkeeping and emit NO IR. The hl-GPU IR
/// is layout-implicit (the executor tracks/transitions resource state itself), so an explicit barrier
/// carries no encoder op — it is honest correctness bookkeeping. An unknown image is skipped. Ported
/// (simplified for the layout-implicit IR) from `command.rs::commit_barriers`.
pub fn cmd_pipeline_barrier(
    dev: &mut Device,
    cb: VkCommandBuffer,
    transitions: &[(VkImage, i32, i32)],
) -> Result<()> {
    // The barrier is only valid while recording; validate the buffer state (records/emits nothing else).
    let _ = recording_mut(dev, cb)?;
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
pub fn cmd_set_event(dev: &mut Device, cb: VkCommandBuffer, event: VkEvent, set: bool) -> Result<()> {
    if !dev.events.contains_key(&event) {
        return Err(GpuError::Invalid("vkCmdSetEvent/ResetEvent: unknown VkEvent"));
    }
    recording_mut(dev, cb)?.deferred.push(DeferredOp::Event { event, set });
    Ok(())
}

/// `vkCmdWaitEvents` — validate the waited events all exist (a wait on an unknown event is a usage
/// error). In this synchronous single-queue model the waited dependency has already resolved by submit
/// completion, so the wait records no op. Ported from `event.rs::vkCmdWaitEvents`.
pub fn cmd_wait_events(dev: &mut Device, cb: VkCommandBuffer, events: &[VkEvent]) -> Result<()> {
    let _ = recording_mut(dev, cb)?;
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
        _ => return Err(GpuError::Invalid("vkCmdResetQueryPool: unknown pool or out-of-range span")),
    }
    recording_mut(dev, cb)?.deferred.push(DeferredOp::QueryReset { pool, first, count });
    Ok(())
}

/// `vkCmdBeginQuery` — open `(pool, query)` on the command buffer (spec §17.4: at most one active query
/// of a type; a second open is ignored). Availability is set at `vkCmdEndQuery`. Errors on a bad pool/
/// index. Ported from `query.rs::vkCmdBeginQuery`.
pub fn cmd_begin_query(dev: &mut Device, cb: VkCommandBuffer, pool: VkQueryPool, query: u32) -> Result<()> {
    match dev.query_pools.get(&pool) {
        Some(p) if query < p.count => {}
        _ => return Err(GpuError::Invalid("vkCmdBeginQuery: unknown pool or query index")),
    }
    let rec = recording_mut(dev, cb)?;
    if rec.active_query.is_none() {
        rec.active_query = Some((pool, query));
    }
    Ok(())
}

/// `vkCmdEndQuery` — close the matching open query, recording it available at a bounded value (a
/// conservative `0`: the synchronous model surfaces no real GPU sample count). Ported from
/// `query.rs::vkCmdEndQuery`.
pub fn cmd_end_query(dev: &mut Device, cb: VkCommandBuffer, pool: VkQueryPool, query: u32) -> Result<()> {
    let rec = recording_mut(dev, cb)?;
    if rec.active_query == Some((pool, query)) {
        rec.active_query = None;
        rec.deferred.push(DeferredOp::QueryEnd { pool, query, value: 0 });
    }
    Ok(())
}

/// `vkCmdWriteTimestamp` — record a timestamp write into `(pool, query)`, resolved to a host-monotonic
/// serial at submit completion. Errors on a bad pool/index. Ported from `query.rs::vkCmdWriteTimestamp`.
pub fn cmd_write_timestamp(dev: &mut Device, cb: VkCommandBuffer, pool: VkQueryPool, query: u32) -> Result<()> {
    match dev.query_pools.get(&pool) {
        Some(p) if query < p.count => {}
        _ => return Err(GpuError::Invalid("vkCmdWriteTimestamp: unknown pool or query index")),
    }
    recording_mut(dev, cb)?.deferred.push(DeferredOp::QueryTimestamp { pool, query });
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
        _ => return Err(GpuError::Invalid("vkCmdCopyQueryPoolResults: unknown pool or out-of-range span")),
    }
    let (dst_ir, bsize, usage) = {
        let b = dev
            .buffers
            .get(&dst_buffer)
            .ok_or(GpuError::Invalid("vkCmdCopyQueryPoolResults: unknown dst VkBuffer"))?;
        (b.ir_id, b.size, b.usage)
    };
    if usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid("vkCmdCopyQueryPoolResults: dst missing COPY_DST usage"));
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
    let dst_size = count as u64 * stride.max(per);
    match dst_offset.checked_add(span) {
        Some(end) if end <= bsize => {}
        _ => return Err(GpuError::OutOfBounds),
    }
    recording_mut(dev, cb)?.deferred.push(DeferredOp::CopyResults {
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
