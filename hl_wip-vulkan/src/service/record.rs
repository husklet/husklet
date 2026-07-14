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
use crate::*;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, ColorAttachment,
};
use hl_gpu::protocol::model::enums::LoadOp;
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

/// `vkCmdEndRenderPass` — close the render pass.
pub fn cmd_end_render_pass(dev: &mut Device, cb: VkCommandBuffer) -> Result<()> {
    let rec = recording_mut(dev, cb)?;
    rec.enc.push(Enc::EndRenderPass);
    rec.in_render_pass = false;
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
