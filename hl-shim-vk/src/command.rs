//! Command-buffer recording + queue submit + synchronization (real bodies), producing dd-gpu IR.
//!
//! Ported from MoltenVK's command + queue objects:
//!   * `Commands/MVKCmdDispatch.mm` — dispatch encodes `dispatchThreadgroups: count
//!     threadsPerThreadgroup: pipeline.threadgroupSize` (group count from the command, threads-per-
//!     group from the shader). Our `vkCmdDispatch` → `Enc::Dispatch{ x,y,z }` (the naga-compiled
//!     kernel supplies `@workgroup_size`), wrapped in a `BeginComputePass`/`EndComputePass`.
//!   * `Commands/MVKCmdDraw.mm` (l.332) — `drawPrimitives: type vertexStart: first vertexCount: n`.
//!     Our `vkCmdDraw` → `Enc::Draw{ vertex_count, first_vertex, ... }`.
//!   * `Commands/MVKCmdRenderPass.mm` — begin sets up the MTLRenderPassDescriptor (load/clear/store
//!     from the render pass; clearColor from `pClearValues`). Our `vkCmdBeginRenderPass` →
//!     `Enc::BeginRenderPass{ color: [ColorAttachment{ texture, load, clear, store }] }`.
//!   * `GPUObjects/MVKQueue.mm` — `submit` encodes the recorded command buffer onto the MTLCommandQueue
//!     and commits. Our `vkQueueSubmit` wraps each recorded encoder in `Cmd::Submit(CommandBuffer{ .. })`
//!     and appends it to the IR log the host executor replays.

use crate::reg::{
    self, CommandBufferState, ImageEvent, ImageSubresourceRange, ImageTransition, RenderPassRec,
};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use hl_shim::ir::*;

/// Key a command buffer's recording by its dispatchable handle pointer.
#[inline]
fn cb_key(cb: VkCommandBuffer) -> usize {
    cb as usize
}

// ---- command-buffer lifecycle (ported from MVKCommandBuffer.mm) ----------------------------------

/// `vkBeginCommandBuffer` — transition a command buffer into the `Recording` state and parse its usage
/// flags. Ported from `MVKCommandBuffer::begin`: `begin` resets the buffer, then reads
/// `ONE_TIME_SUBMIT` (→ not reusable: becomes `Invalid` after one execution) and `SIMULTANEOUS_USE`
/// (→ may be resubmitted while `Pending`). A buffer that is currently `Pending` (in flight) may not be
/// begun (`VK_NOT_READY`).
#[no_mangle]
pub extern "C" fn vkBeginCommandBuffer(
    command_buffer: VkCommandBuffer,
    p_begin_info: *const vk::CommandBufferBeginInfo,
) -> VkResult {
    let flags = unsafe { p_begin_info.as_ref() }
        .map(|bi| bi.flags)
        .unwrap_or_default();
    let one_time_submit = flags.contains(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    let simultaneous_use = flags.contains(vk::CommandBufferUsageFlags::SIMULTANEOUS_USE);
    let mut s = reg::lock();
    let cb = s.cmdbufs.entry(cb_key(command_buffer)).or_insert_with(reg::CmdBufRec::initial);
    if cb.state == CommandBufferState::Pending {
        return VK_NOT_READY; // cannot record a command buffer still in flight
    }
    cb.reset_recording(); // MVKCommandBuffer::begin calls reset(0) first
    cb.state = CommandBufferState::Recording;
    cb.one_time_submit = one_time_submit;
    cb.simultaneous_use = simultaneous_use;
    cb.was_executed = false;
    VK_SUCCESS
}

/// `vkEndCommandBuffer` — finish recording: `Recording` → `Executable`. Ending a buffer that is not
/// recording is an error (`VK_NOT_READY`), matching `MVKCommandBuffer`'s `_canAcceptCommands` gate.
#[no_mangle]
pub extern "C" fn vkEndCommandBuffer(command_buffer: VkCommandBuffer) -> VkResult {
    let mut s = reg::lock();
    match s.recording_mut(cb_key(command_buffer)) {
        Some(cb) if cb.state == CommandBufferState::Recording => {
            cb.state = CommandBufferState::Executable;
            VK_SUCCESS
        }
        _ => VK_NOT_READY,
    }
}

/// `vkResetCommandBuffer` — return a buffer to `Initial`, dropping its recorded commands. A `Pending`
/// buffer must not be reset (external synchronization); we reject it rather than corrupt in-flight work.
#[no_mangle]
pub extern "C" fn vkResetCommandBuffer(command_buffer: VkCommandBuffer, _flags: u32) -> VkResult {
    let mut s = reg::lock();
    if let Some(cb) = s.cmdbufs.get_mut(&cb_key(command_buffer)) {
        if cb.state == CommandBufferState::Pending {
            return VK_NOT_READY;
        }
        cb.reset_recording();
        cb.state = CommandBufferState::Initial;
        cb.was_executed = false;
    }
    VK_SUCCESS
}

/// `vkResetCommandPool` — reset every command buffer recorded under this device back to `Initial`
/// (the pool-wide reset). Pending buffers are left in flight. Ported from `MVKCommandPool::reset`.
#[no_mangle]
pub extern "C" fn vkResetCommandPool(_device: VkDevice, _command_pool: VkCommandPool, _flags: u32) -> VkResult {
    let mut s = reg::lock();
    for cb in s.cmdbufs.values_mut() {
        if cb.state != CommandBufferState::Pending {
            cb.reset_recording();
            cb.state = CommandBufferState::Initial;
            cb.was_executed = false;
        }
    }
    VK_SUCCESS
}

// ---- state-setting commands ----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdBindPipeline(
    command_buffer: VkCommandBuffer,
    pipeline_bind_point: i32,
    pipeline: VkPipeline,
) {
    let mut s = reg::lock();
    let ir = s.pipelines.get(&pipeline).map(|p| p.ir_id);
    let kind = if pipeline_bind_point == vk::PipelineBindPoint::COMPUTE.as_raw() {
        1u8
    } else {
        0u8
    };
    if let (Some(ir), Some(cb)) = (ir, s.recording_mut(cb_key(command_buffer))) {
        cb.bound_pipeline = Some(ir);
        cb.bound_pipeline_kind = Some(kind);
        // Graphics: emit SetPipeline as soon as it's bound inside a render pass (matches the host order).
        if kind == 0 && cb.in_render_pass && !cb.pipeline_set_in_pass {
            cb.enc.push(Enc::SetPipeline(ir));
            cb.pipeline_set_in_pass = true;
        }
    }
}

/// `vkCmdBindDescriptorSets` — bind descriptor sets, consuming `pDynamicOffsets` and honouring the
/// pipeline `layout` the sets must be compatible with. Ported from `MVKCmdBindDescriptorSets` +
/// `MVKPipelineLayout::bindDescriptorSets`: dynamic offsets are consumed across the bound sets in set
/// order, then by ascending dynamic-buffer binding within each set (spec §14.2.7), and added to the
/// buffer offset of the matching binding when the IR bind group is built.
#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets(
    command_buffer: VkCommandBuffer,
    _pipeline_bind_point: i32,
    layout: VkPipelineLayout,
    first_set: u32,
    descriptor_set_count: u32,
    p_descriptor_sets: *const VkDescriptorSet,
    dynamic_offset_count: u32,
    p_dynamic_offsets: *const u32,
) {
    if p_descriptor_sets.is_null() {
        return;
    }
    // The pipeline layout the bound sets are declared against (compatibility is by set-layout; the
    // sets carry their own layout in this bring-up model, so we thread the handle through explicitly).
    let _pipeline_layout = layout;
    let sets = unsafe { core::slice::from_raw_parts(p_descriptor_sets, descriptor_set_count as usize) };
    let dyn_offsets: &[u32] = if p_dynamic_offsets.is_null() {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(p_dynamic_offsets, dynamic_offset_count as usize) }
    };
    let mut dyn_cursor = 0usize; // global cursor across all bound sets
    let mut s = reg::lock();
    for (i, &dset) in sets.iter().enumerate() {
        let set_index = first_set + i as u32;
        // Snapshot the set's (binding -> buffer) table and the layout handle (borrow ends here).
        let Some(rec) = s.dsets.get(&dset) else { continue };
        let layout_handle = rec.layout;
        let mut pairs: Vec<(u32, (u64, u64, u64))> = rec.buffers.iter().map(|(b, v)| (*b, *v)).collect();
        pairs.sort_by_key(|(b, _)| *b);
        // Consume this set's dynamic offsets (its layout's dynamic buffer bindings, ascending).
        let mut extra: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        let dyn_bindings = s
            .descriptor_set_layouts
            .get(&layout_handle)
            .map(|l| l.dynamic_bindings())
            .unwrap_or_default();
        for db in dyn_bindings {
            if dyn_cursor < dyn_offsets.len() {
                extra.insert(db, dyn_offsets[dyn_cursor] as u64);
                dyn_cursor += 1;
            }
        }
        let mut entries: Vec<BindEntry> = Vec::new();
        for (binding, (buf_handle, offset, size)) in pairs {
            if let Some(b) = s.buffers.get(&buf_handle) {
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
        let ir_id = s.alloc_ir();
        s.record(Cmd::CreateBindGroup(
            ir_id,
            BindGroupDesc {
                set: set_index,
                entries,
            },
        ));
        if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
            cb.pending_bind_groups.push((set_index, ir_id));
        }
    }
}

#[no_mangle]
pub extern "C" fn vkCmdBindVertexBuffers(
    command_buffer: VkCommandBuffer,
    first_binding: u32,
    binding_count: u32,
    p_buffers: *const VkBuffer,
    p_offsets: *const u64,
) {
    if p_buffers.is_null() {
        return;
    }
    let buffers = unsafe { core::slice::from_raw_parts(p_buffers, binding_count as usize) };
    let mut s = reg::lock();
    for (i, &bh) in buffers.iter().enumerate() {
        let Some(ir) = s.buffers.get(&bh).map(|b| b.ir_id) else { continue };
        let offset = if p_offsets.is_null() {
            0
        } else {
            unsafe { *p_offsets.add(i) }
        };
        if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
            cb.enc.push(Enc::SetVertexBuffer {
                slot: first_binding + i as u32,
                buffer: ir,
                offset,
            });
        }
    }
}

#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer(
    command_buffer: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    index_type: i32,
) {
    let mut s = reg::lock();
    let Some(ir) = s.buffers.get(&buffer).map(|b| b.ir_id) else { return };
    // VkIndexType: 0 = UINT16, 1 = UINT32.
    let format = if index_type == vk::IndexType::UINT16.as_raw() {
        IndexFormat::U16
    } else {
        IndexFormat::U32
    };
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.push(Enc::SetIndexBuffer {
            buffer: ir,
            offset,
            format,
        });
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetViewport(
    command_buffer: VkCommandBuffer,
    _first_viewport: u32,
    viewport_count: u32,
    p_viewports: *const vk::Viewport,
) {
    if p_viewports.is_null() || viewport_count == 0 {
        return;
    }
    let v = unsafe { &*p_viewports };
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        cb.enc.push(Enc::SetViewport {
            x: v.x,
            y: v.y,
            w: v.width,
            h: v.height,
            min_depth: v.min_depth,
            max_depth: v.max_depth,
        });
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetScissor(
    command_buffer: VkCommandBuffer,
    _first_scissor: u32,
    scissor_count: u32,
    p_scissors: *const vk::Rect2D,
) {
    if p_scissors.is_null() || scissor_count == 0 {
        return;
    }
    let r = unsafe { &*p_scissors };
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        cb.enc.push(Enc::SetScissor {
            x: r.offset.x.max(0) as u32,
            y: r.offset.y.max(0) as u32,
            w: r.extent.width,
            h: r.extent.height,
        });
    }
}

// ---- compute -------------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdDispatch(
    command_buffer: VkCommandBuffer,
    group_count_x: u32,
    group_count_y: u32,
    group_count_z: u32,
) {
    let mut s = reg::lock();
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        let pipeline = cb.bound_pipeline;
        let groups: Vec<(u32, u32)> = cb.pending_bind_groups.clone();
        cb.enc.push(Enc::BeginComputePass);
        if let Some(p) = pipeline {
            cb.enc.push(Enc::SetPipeline(p));
        }
        for (index, group) in groups {
            cb.enc.push(Enc::SetBindGroup { index, group });
        }
        cb.enc.push(Enc::Dispatch {
            x: group_count_x,
            y: group_count_y,
            z: group_count_z,
        });
        cb.enc.push(Enc::EndComputePass);
    }
}

// ---- graphics render pass ------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdBeginRenderPass(
    command_buffer: VkCommandBuffer,
    p_render_pass_begin: *const vk::RenderPassBeginInfo,
    _contents: i32,
) {
    let Some(bi) = (unsafe { p_render_pass_begin.as_ref() }) else {
        return;
    };
    let mut s = reg::lock();
    // Validate the named render pass + framebuffer exist — an invalid/missing object must NOT be
    // silently substituted with a zero-sized default-clear pass (MoltenVK resolves the concrete
    // MVKRenderPass/MVKFramebuffer; a missing one is a usage error). Record nothing on a bad handle.
    let Some(rp) = s.render_passes.get(&bi.render_pass.as_raw()).map(|r| RenderPassRec {
        color_format: r.color_format,
        color_load_clear: r.color_load_clear,
        clear: r.clear,
        color_store: r.color_store,
        initial_layout: r.initial_layout,
        subpass_layout: r.subpass_layout,
        final_layout: r.final_layout,
    }) else {
        return;
    };
    let Some((fb_w, fb_h, color_view)) =
        s.framebuffers.get(&bi.framebuffer.as_raw()).map(|f| (f.width, f.height, f.color_view))
    else {
        return;
    };
    // The framebuffer's color attachment must resolve to a real image-view → image → IR texture.
    let Some((image, range, tex)) = color_view
        .and_then(|v| s.image_views.get(&v).copied())
        .and_then(|iv| s.images.get(&iv.image).map(|im| (iv.image, iv.range, im.ir_id)))
    else {
        return;
    };

    // Clear color from pClearValues[0].
    let clear = if bi.clear_value_count > 0 && !bi.p_clear_values.is_null() {
        let cv = unsafe { &*bi.p_clear_values };
        unsafe { cv.color.float32 }
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    // Load/store come from the validated render pass (no default substitution).
    let load = if rp.color_load_clear { LoadOp::Clear } else { LoadOp::Load };
    let store = rp.color_store;

    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.image_events.push(ImageEvent::RenderBegin {
            image,
            range,
            initial_layout: rp.initial_layout,
            subpass_layout: rp.subpass_layout,
        });
        cb.active_render_image = Some((image, range, rp.subpass_layout, rp.final_layout));
        cb.enc.push(Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: tex,
                load,
                clear,
                store,
            }],
            depth: None,
        });
        // Auto-cover the render area with a full viewport + scissor (so a pipeline with static
        // viewport state still rasterizes correctly without the app emitting dynamic state).
        cb.enc.push(Enc::SetViewport {
            x: 0.0,
            y: 0.0,
            w: fb_w as f32,
            h: fb_h as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        });
        cb.enc.push(Enc::SetScissor {
            x: 0,
            y: 0,
            w: fb_w,
            h: fb_h,
        });
        cb.in_render_pass = true;
        cb.pipeline_set_in_pass = false;
    }
}

#[no_mangle]
pub extern "C" fn vkCmdEndRenderPass(command_buffer: VkCommandBuffer) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        if let Some((image, range, subpass_layout, final_layout)) = cb.active_render_image.take() {
            cb.image_events.push(ImageEvent::RenderEnd {
                image,
                range,
                subpass_layout,
                final_layout,
            });
        }
        cb.enc.push(Enc::EndRenderPass);
        cb.in_render_pass = false;
    }
}

#[no_mangle]
pub extern "C" fn vkCmdDraw(
    command_buffer: VkCommandBuffer,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        if let (false, Some(p)) = (cb.pipeline_set_in_pass, cb.bound_pipeline) {
            cb.enc.push(Enc::SetPipeline(p));
            cb.pipeline_set_in_pass = true;
        }
        // Bind the descriptor sets recorded for this draw (graphics parity with vkCmdDispatch): a set's
        // UBO/texture/sampler bindings sit in `pending_bind_groups` until a draw/dispatch flushes them.
        // Without this the vertex/fragment shaders read unbound resources (a textured cube renders blank).
        for (index, group) in cb.pending_bind_groups.clone() {
            cb.enc.push(Enc::SetBindGroup { index, group });
        }
        cb.enc.push(Enc::Draw {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        });
    }
}

#[no_mangle]
pub extern "C" fn vkCmdDrawIndexed(
    command_buffer: VkCommandBuffer,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        if let (false, Some(p)) = (cb.pipeline_set_in_pass, cb.bound_pipeline) {
            cb.enc.push(Enc::SetPipeline(p));
            cb.pipeline_set_in_pass = true;
        }
        // Flush recorded descriptor sets before the draw (see vkCmdDraw).
        for (index, group) in cb.pending_bind_groups.clone() {
            cb.enc.push(Enc::SetBindGroup { index, group });
        }
        cb.enc.push(Enc::DrawIndexed {
            index_count,
            instance_count,
            first_index,
            base_vertex: vertex_offset,
            first_instance,
        });
    }
}

// ---- transfer ------------------------------------------------------------------------------------

fn supported_layout(layout: vk::ImageLayout, allow_undefined: bool) -> bool {
    (allow_undefined && layout == vk::ImageLayout::UNDEFINED)
        || matches!(
            layout,
            vk::ImageLayout::GENERAL
                | vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
                | vk::ImageLayout::TRANSFER_SRC_OPTIMAL
                | vk::ImageLayout::TRANSFER_DST_OPTIMAL
                | vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                | vk::ImageLayout::PRESENT_SRC_KHR
        )
}

fn resolve_range(image: &reg::ImageRec, raw: vk::ImageSubresourceRange) -> Option<ImageSubresourceRange> {
    let level_count = if raw.level_count == vk::REMAINING_MIP_LEVELS {
        image.mip_levels.checked_sub(raw.base_mip_level)?
    } else {
        raw.level_count
    };
    let layer_count = if raw.layer_count == vk::REMAINING_ARRAY_LAYERS {
        image.array_layers.checked_sub(raw.base_array_layer)?
    } else {
        raw.layer_count
    };
    let range = ImageSubresourceRange {
        aspect_mask: raw.aspect_mask.as_raw(),
        base_mip_level: raw.base_mip_level,
        level_count,
        base_array_layer: raw.base_array_layer,
        layer_count,
    };
    (range.aspect_mask == image.aspect_mask
        && level_count != 0
        && layer_count != 0
        && raw.base_mip_level.checked_add(level_count).is_some_and(|end| end <= image.mip_levels)
        && raw.base_array_layer.checked_add(layer_count).is_some_and(|end| end <= image.array_layers))
    .then_some(range)
}

/// Record legacy image barriers as Vulkan state transitions. Metal supplies ordered execution for
/// this synchronous single-queue slice; the state transitions themselves are validated and applied
/// atomically at queue submission, not while command buffers are merely recorded.
#[no_mangle]
pub extern "C" fn vkCmdPipelineBarrier(
    command_buffer: VkCommandBuffer,
    src_stage_mask: vk::PipelineStageFlags,
    dst_stage_mask: vk::PipelineStageFlags,
    _dependency_flags: vk::DependencyFlags,
    memory_barrier_count: u32,
    p_memory_barriers: *const vk::MemoryBarrier,
    buffer_memory_barrier_count: u32,
    p_buffer_memory_barriers: *const vk::BufferMemoryBarrier,
    image_memory_barrier_count: u32,
    p_image_memory_barriers: *const vk::ImageMemoryBarrier,
) {
    if (memory_barrier_count != 0 && p_memory_barriers.is_null())
        || (buffer_memory_barrier_count != 0 && p_buffer_memory_barriers.is_null())
        || (image_memory_barrier_count != 0 && p_image_memory_barriers.is_null())
    {
        return;
    }
    record_image_barriers(
        command_buffer,
        src_stage_mask,
        dst_stage_mask,
        image_memory_barrier_count,
        p_image_memory_barriers,
    );
}

/// Parse a `VkImageMemoryBarrier` array into validated [`ImageTransition`]s and record them as one
/// atomic [`ImageEvent::Barriers`] group on a recording command buffer. Shared by `vkCmdPipelineBarrier`
/// and `vkCmdWaitEvents` (both carry the same image-barrier arrays; in this synchronous single-queue
/// model an event's dependency resolves by submit completion, so its barriers join the same submit-time
/// transition validation). A structurally invalid barrier records nothing (no partial mutation).
pub(crate) fn record_image_barriers(
    command_buffer: VkCommandBuffer,
    src_stage_mask: vk::PipelineStageFlags,
    dst_stage_mask: vk::PipelineStageFlags,
    image_memory_barrier_count: u32,
    p_image_memory_barriers: *const vk::ImageMemoryBarrier,
) {
    let barriers = if image_memory_barrier_count == 0 || p_image_memory_barriers.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(p_image_memory_barriers, image_memory_barrier_count as usize) }
    };
    // Legacy (Vulkan 1.0) form: the stage masks are per-`vkCmdPipelineBarrier`, shared by every barrier.
    let inputs: Vec<BarrierInput> = barriers
        .iter()
        .map(|b| BarrierInput {
            image: b.image.as_raw(),
            old_layout: b.old_layout,
            new_layout: b.new_layout,
            src_access: b.src_access_mask.as_raw(),
            dst_access: b.dst_access_mask.as_raw(),
            src_stage: src_stage_mask.as_raw(),
            dst_stage: dst_stage_mask.as_raw(),
            src_qf: b.src_queue_family_index,
            dst_qf: b.dst_queue_family_index,
            range: b.subresource_range,
        })
        .collect();
    commit_barriers(command_buffer, &inputs);
}

/// One resolved image barrier, common to the legacy (`VkImageMemoryBarrier` + shared stage masks) and
/// synchronization2 (`VkImageMemoryBarrier2` with per-barrier 64-bit stage/access masks) forms.
struct BarrierInput {
    image: u64,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_access: u32,
    dst_access: u32,
    src_stage: u32,
    dst_stage: u32,
    src_qf: u32,
    dst_qf: u32,
    range: vk::ImageSubresourceRange,
}

/// Validate a set of resolved image barriers and record them as one atomic [`ImageEvent::Barriers`]
/// group on a recording command buffer (outside a render pass). Any structurally invalid barrier
/// (unknown image, out-of-range subresource, unsupported layout, or a cross-family transfer to a
/// non-existent queue family) records nothing — no partial mutation. Shared by the legacy and sync2
/// barrier entry points and `vkCmdWaitEvents`, so both track per-mip/layer subresource state and
/// queue ownership through the same submit-time validation.
fn commit_barriers(command_buffer: VkCommandBuffer, inputs: &[BarrierInput]) {
    let mut s = reg::lock();
    let key = cb_key(command_buffer);
    if !s.cmdbufs.get(&key).is_some_and(|cb| cb.state == CommandBufferState::Recording && !cb.in_render_pass) {
        return;
    }
    let mut transitions = Vec::with_capacity(inputs.len());
    for b in inputs {
        let Some(image) = s.images.get(&b.image) else {
            return;
        };
        let Some(range) = resolve_range(image, b.range) else {
            return;
        };
        if !supported_layout(b.old_layout, true) || !supported_layout(b.new_layout, false) {
            return;
        }
        let ignored = vk::QUEUE_FAMILY_IGNORED;
        // Our device exposes one queue family (index 0): a transfer is valid only when both are IGNORED
        // (no ownership transfer) or both name the sole family 0 (explicit ownership, tracked at submit).
        let queue_ok = (b.src_qf == ignored && b.dst_qf == ignored) || (b.src_qf == 0 && b.dst_qf == 0);
        if !queue_ok {
            return;
        }
        transitions.push(ImageTransition {
            image: b.image,
            range,
            old_layout: b.old_layout.as_raw(),
            new_layout: b.new_layout.as_raw(),
            src_access: b.src_access,
            dst_access: b.dst_access,
            src_stage: b.src_stage,
            dst_stage: b.dst_stage,
            src_queue_family: b.src_qf,
            dst_queue_family: b.dst_qf,
        });
    }
    if !transitions.is_empty() {
        s.cmdbufs
            .get_mut(&key)
            .expect("validated recording command buffer")
            .image_events
            .push(ImageEvent::Barriers(transitions));
    }
}

/// `vkCmdPipelineBarrier2` (synchronization2 / core 1.3) — the `VkDependencyInfo` form: each
/// `VkImageMemoryBarrier2` carries its own 64-bit `srcStageMask`/`dstStageMask`/`src|dstAccessMask`
/// inline (rather than the single per-call stage masks of the legacy form). We lower its image
/// barriers through the SAME validated submit-time subresource/ownership model as the legacy path
/// (the low 32 bits of the sync2 masks carry the legacy-compatible stage/access bits the tracker
/// records). Ported from `MVKCmdPipelineBarrier` (which unifies the 1.0 and sync2 barrier encodings).
#[no_mangle]
pub extern "C" fn vkCmdPipelineBarrier2(
    command_buffer: VkCommandBuffer,
    p_dependency_info: *const vk::DependencyInfo,
) {
    let Some(dep) = (unsafe { p_dependency_info.as_ref() }) else {
        return;
    };
    let barriers = if dep.image_memory_barrier_count == 0 || dep.p_image_memory_barriers.is_null() {
        &[][..]
    } else {
        unsafe {
            core::slice::from_raw_parts(dep.p_image_memory_barriers, dep.image_memory_barrier_count as usize)
        }
    };
    let inputs: Vec<BarrierInput> = barriers
        .iter()
        .map(|b| BarrierInput {
            image: b.image.as_raw(),
            old_layout: b.old_layout,
            new_layout: b.new_layout,
            // sync2 masks are 64-bit; the low 32 bits carry the legacy-compatible access/stage bits the
            // subresource tracker records (it keys on layout + ownership, identical across both forms).
            src_access: b.src_access_mask.as_raw() as u32,
            dst_access: b.dst_access_mask.as_raw() as u32,
            src_stage: b.src_stage_mask.as_raw() as u32,
            dst_stage: b.dst_stage_mask.as_raw() as u32,
            src_qf: b.src_queue_family_index,
            dst_qf: b.dst_queue_family_index,
            range: b.subresource_range,
        })
        .collect();
    commit_barriers(command_buffer, &inputs);
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBuffer(
    command_buffer: VkCommandBuffer,
    src_buffer: VkBuffer,
    dst_buffer: VkBuffer,
    region_count: u32,
    p_regions: *const vk::BufferCopy,
) {
    if p_regions.is_null() {
        return;
    }
    let mut s = reg::lock();
    let (Some(src), Some(dst)) = (
        s.buffers.get(&src_buffer).map(|b| b.ir_id),
        s.buffers.get(&dst_buffer).map(|b| b.ir_id),
    ) else {
        return;
    };
    let regions = unsafe { core::slice::from_raw_parts(p_regions, region_count as usize) };
    let encs: Vec<Enc> = regions
        .iter()
        .map(|r| Enc::CopyBufferToBuffer {
            src,
            src_offset: r.src_offset,
            dst,
            dst_offset: r.dst_offset,
            size: r.size,
        })
        .collect();
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
    }
}

fn transfer_layout_ok(layout: vk::ImageLayout, write: bool) -> bool {
    layout == vk::ImageLayout::GENERAL
        || (write && layout == vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        || (!write && layout == vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
}

fn layers_range(image: &reg::ImageRec, layers: vk::ImageSubresourceLayers) -> Option<ImageSubresourceRange> {
    let range = ImageSubresourceRange {
        aspect_mask: layers.aspect_mask.as_raw(),
        base_mip_level: layers.mip_level,
        level_count: 1,
        base_array_layer: layers.base_array_layer,
        layer_count: layers.layer_count,
    };
    (range.aspect_mask == image.aspect_mask
        && range.layer_count != 0
        && range.base_mip_level < image.mip_levels
        && range.base_array_layer.checked_add(range.layer_count).is_some_and(|end| end <= image.array_layers))
    .then_some(range)
}

fn mip_extent(image: &reg::ImageRec, mip: u32) -> Option<(u32, u32)> {
    (mip < image.mip_levels).then(|| ((image.width >> mip).max(1), (image.height >> mip).max(1)))
}

fn checked_region(
    image: &reg::ImageRec,
    layers: vk::ImageSubresourceLayers,
    offset: vk::Offset3D,
    extent: vk::Extent3D,
) -> Option<(ImageSubresourceRange, TextureSubresource, Origin3d, Extent3d)> {
    let range = layers_range(image, layers)?;
    if layers.layer_count != 1 || offset.x < 0 || offset.y < 0 || offset.z != 0 || extent.depth != 1 {
        return None;
    }
    let (mw, mh) = mip_extent(image, layers.mip_level)?;
    let origin = Origin3d { x: offset.x as u32, y: offset.y as u32, z: 0 };
    if extent.width == 0
        || extent.height == 0
        || origin.x.checked_add(extent.width).is_none_or(|end| end > mw)
        || origin.y.checked_add(extent.height).is_none_or(|end| end > mh)
    {
        return None;
    }
    Some((
        range,
        TextureSubresource {
            mip: layers.mip_level,
            layer: layers.base_array_layer,
            aspect: TextureAspect::All,
        },
        origin,
        Extent3d { width: extent.width, height: extent.height, depth: 1 },
    ))
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBufferToImage(
    command_buffer: VkCommandBuffer,
    src_buffer: VkBuffer,
    dst_image: VkImage,
    dst_image_layout: vk::ImageLayout,
    region_count: u32,
    p_regions: *const vk::BufferImageCopy,
) {
    if region_count == 0 || p_regions.is_null() || !transfer_layout_ok(dst_image_layout, true) {
        return;
    }
    let regions = unsafe { core::slice::from_raw_parts(p_regions, region_count as usize) };
    let mut s = reg::lock();
    let (Some(buffer), Some(image)) = (s.buffers.get(&src_buffer), s.images.get(&dst_image)) else {
        return;
    };
    if buffer.usage & buffer_usage::COPY_SRC == 0
        || image.usage & vk::ImageUsageFlags::TRANSFER_DST.as_raw() == 0
        || image.sample_count != 1
    {
        return;
    }
    let mut encs = Vec::with_capacity(regions.len());
    let mut events = Vec::with_capacity(regions.len());
    for region in regions {
        let Some((range, sub, origin, extent)) =
            checked_region(image, region.image_subresource, region.image_offset, region.image_extent)
        else {
            return;
        };
        if origin != Origin3d::default() || sub.layer != 0 {
            return;
        }
        let row_texels = if region.buffer_row_length == 0 { extent.width } else { region.buffer_row_length };
        let image_rows = if region.buffer_image_height == 0 { extent.height } else { region.buffer_image_height };
        if row_texels < extent.width || image_rows < extent.height {
            return;
        }
        let bytes_per_row = match row_texels.checked_mul(4) {
            Some(value) => value,
            None => return,
        };
        let span = match (bytes_per_row as u64)
            .checked_mul(extent.height.saturating_sub(1) as u64)
            .and_then(|value| value.checked_add(extent.width as u64 * 4))
            .and_then(|value| value.checked_add(region.buffer_offset))
        {
            Some(value) if value <= buffer.size => value,
            _ => return,
        };
        let _ = span;
        encs.push(Enc::CopyBufferToTexture {
            src: buffer.ir_id,
            src_offset: region.buffer_offset,
            bytes_per_row,
            dst: image.ir_id,
            mip: sub.mip,
            width: extent.width,
            height: extent.height,
        });
        events.push(ImageEvent::TransferUse {
            image: dst_image,
            range,
            required_layout: dst_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_WRITE.as_raw(),
        });
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
        cb.image_events.extend(events);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImageToBuffer(
    command_buffer: VkCommandBuffer,
    src_image: VkImage,
    src_image_layout: vk::ImageLayout,
    dst_buffer: VkBuffer,
    region_count: u32,
    p_regions: *const vk::BufferImageCopy,
) {
    if region_count == 0 || p_regions.is_null() || !transfer_layout_ok(src_image_layout, false) {
        return;
    }
    let regions = unsafe { core::slice::from_raw_parts(p_regions, region_count as usize) };
    let mut s = reg::lock();
    let (Some(image), Some(buffer)) = (s.images.get(&src_image), s.buffers.get(&dst_buffer)) else {
        return;
    };
    if image.usage & vk::ImageUsageFlags::TRANSFER_SRC.as_raw() == 0
        || buffer.usage & buffer_usage::COPY_DST == 0
        || image.sample_count != 1
    {
        return;
    }
    let mut encs = Vec::with_capacity(regions.len());
    let mut events = Vec::with_capacity(regions.len());
    for region in regions {
        let Some((range, sub, origin, extent)) =
            checked_region(image, region.image_subresource, region.image_offset, region.image_extent)
        else {
            return;
        };
        if origin != Origin3d::default() || sub.layer != 0 {
            return;
        }
        let row_texels = if region.buffer_row_length == 0 { extent.width } else { region.buffer_row_length };
        let image_rows = if region.buffer_image_height == 0 { extent.height } else { region.buffer_image_height };
        if row_texels < extent.width || image_rows < extent.height {
            return;
        }
        let bytes_per_row = match row_texels.checked_mul(4) {
            Some(value) => value,
            None => return,
        };
        let end = (bytes_per_row as u64)
            .checked_mul(extent.height.saturating_sub(1) as u64)
            .and_then(|value| value.checked_add(extent.width as u64 * 4))
            .and_then(|value| value.checked_add(region.buffer_offset));
        if end.is_none_or(|value| value > buffer.size) {
            return;
        }
        encs.push(Enc::CopyTextureToBuffer {
            src: image.ir_id,
            mip: sub.mip,
            width: extent.width,
            height: extent.height,
            dst: buffer.ir_id,
            dst_offset: region.buffer_offset,
            bytes_per_row,
        });
        events.push(ImageEvent::TransferUse {
            image: src_image,
            range,
            required_layout: src_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_READ.as_raw(),
        });
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
        cb.image_events.extend(events);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImage(
    command_buffer: VkCommandBuffer,
    src_image: VkImage,
    src_image_layout: vk::ImageLayout,
    dst_image: VkImage,
    dst_image_layout: vk::ImageLayout,
    region_count: u32,
    p_regions: *const vk::ImageCopy,
) {
    if region_count == 0
        || p_regions.is_null()
        || !transfer_layout_ok(src_image_layout, false)
        || !transfer_layout_ok(dst_image_layout, true)
    {
        return;
    }
    let regions = unsafe { core::slice::from_raw_parts(p_regions, region_count as usize) };
    let mut s = reg::lock();
    let (Some(src), Some(dst)) = (s.images.get(&src_image), s.images.get(&dst_image)) else {
        return;
    };
    if src.format != dst.format
        || src.sample_count != 1
        || dst.sample_count != 1
        || src.usage & vk::ImageUsageFlags::TRANSFER_SRC.as_raw() == 0
        || dst.usage & vk::ImageUsageFlags::TRANSFER_DST.as_raw() == 0
    {
        return;
    }
    let mut encs = Vec::with_capacity(regions.len());
    let mut events = Vec::with_capacity(regions.len() * 2);
    for region in regions {
        let Some((src_range, src_sub, src_origin, extent)) =
            checked_region(src, region.src_subresource, region.src_offset, region.extent)
        else {
            return;
        };
        let Some((dst_range, dst_sub, dst_origin, dst_extent)) =
            checked_region(dst, region.dst_subresource, region.dst_offset, region.extent)
        else {
            return;
        };
        if extent != dst_extent {
            return;
        }
        if src_image == dst_image
            && src_sub == dst_sub
            && src_origin.x < dst_origin.x + extent.width
            && dst_origin.x < src_origin.x + extent.width
            && src_origin.y < dst_origin.y + extent.height
            && dst_origin.y < src_origin.y + extent.height
        {
            return;
        }
        encs.push(Enc::CopyTextureToTexture {
            src: src.ir_id,
            src_sub,
            src_origin,
            dst: dst.ir_id,
            dst_sub,
            dst_origin,
            extent,
        });
        events.push(ImageEvent::TransferUse {
            image: src_image,
            range: src_range,
            required_layout: src_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_READ.as_raw(),
        });
        events.push(ImageEvent::TransferUse {
            image: dst_image,
            range: dst_range,
            required_layout: dst_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_WRITE.as_raw(),
        });
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
        cb.image_events.extend(events);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdBlitImage(
    command_buffer: VkCommandBuffer,
    src_image: VkImage,
    src_image_layout: vk::ImageLayout,
    dst_image: VkImage,
    dst_image_layout: vk::ImageLayout,
    region_count: u32,
    p_regions: *const vk::ImageBlit,
    filter: vk::Filter,
) {
    if region_count == 0
        || p_regions.is_null()
        || !transfer_layout_ok(src_image_layout, false)
        || !transfer_layout_ok(dst_image_layout, true)
        || !matches!(filter, vk::Filter::NEAREST | vk::Filter::LINEAR)
    {
        return;
    }
    let regions = unsafe { core::slice::from_raw_parts(p_regions, region_count as usize) };
    let mut s = reg::lock();
    let (Some(src), Some(dst)) = (s.images.get(&src_image), s.images.get(&dst_image)) else {
        return;
    };
    if src.format != dst.format
        || src.sample_count != 1
        || dst.sample_count != 1
        || src_image == dst_image
        || src.usage & vk::ImageUsageFlags::TRANSFER_SRC.as_raw() == 0
        || dst.usage & vk::ImageUsageFlags::TRANSFER_DST.as_raw() == 0
    {
        return;
    }
    let mut encs = Vec::with_capacity(regions.len());
    let mut events = Vec::with_capacity(regions.len() * 2);
    for region in regions {
        let [s0, s1] = region.src_offsets;
        let [d0, d1] = region.dst_offsets;
        if s1.x <= s0.x || s1.y <= s0.y || s0.z != 0 || s1.z != 1
            || d1.x <= d0.x || d1.y <= d0.y || d0.z != 0 || d1.z != 1
        {
            return; // reversed/3D blits need signed origins in the shared IR
        }
        let src_extent_vk = vk::Extent3D {
            width: (s1.x - s0.x) as u32,
            height: (s1.y - s0.y) as u32,
            depth: 1,
        };
        let dst_extent_vk = vk::Extent3D {
            width: (d1.x - d0.x) as u32,
            height: (d1.y - d0.y) as u32,
            depth: 1,
        };
        let Some((src_range, src_sub, src_origin, src_extent)) =
            checked_region(src, region.src_subresource, s0, src_extent_vk)
        else {
            return;
        };
        let Some((dst_range, dst_sub, dst_origin, dst_extent)) =
            checked_region(dst, region.dst_subresource, d0, dst_extent_vk)
        else {
            return;
        };
        encs.push(Enc::BlitTexture {
            src: src.ir_id,
            src_sub,
            src_origin,
            src_extent,
            dst: dst.ir_id,
            dst_sub,
            dst_origin,
            dst_extent,
            filter: if filter == vk::Filter::LINEAR { Filter::Linear } else { Filter::Nearest },
        });
        events.push(ImageEvent::TransferUse {
            image: src_image,
            range: src_range,
            required_layout: src_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_READ.as_raw(),
        });
        events.push(ImageEvent::TransferUse {
            image: dst_image,
            range: dst_range,
            required_layout: dst_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_WRITE.as_raw(),
        });
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
        cb.image_events.extend(events);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdResolveImage(
    command_buffer: VkCommandBuffer,
    src_image: VkImage,
    src_image_layout: vk::ImageLayout,
    dst_image: VkImage,
    dst_image_layout: vk::ImageLayout,
    region_count: u32,
    p_regions: *const vk::ImageResolve,
) {
    if region_count == 0 || p_regions.is_null()
        || !transfer_layout_ok(src_image_layout, false)
        || !transfer_layout_ok(dst_image_layout, true)
    {
        return;
    }
    let regions = unsafe { core::slice::from_raw_parts(p_regions, region_count as usize) };
    let mut s = reg::lock();
    let (Some(src), Some(dst)) = (s.images.get(&src_image), s.images.get(&dst_image)) else { return };
    if src_image == dst_image || src.format != dst.format || src.sample_count <= 1 || dst.sample_count != 1
        || src.usage & vk::ImageUsageFlags::TRANSFER_SRC.as_raw() == 0
        || dst.usage & vk::ImageUsageFlags::TRANSFER_DST.as_raw() == 0
    {
        return;
    }
    let mut encs = Vec::with_capacity(regions.len());
    let mut events = Vec::with_capacity(regions.len() * 2);
    for region in regions {
        let Some((src_range, src_sub, src_origin, extent)) =
            checked_region(src, region.src_subresource, region.src_offset, region.extent) else { return };
        let Some((dst_range, dst_sub, dst_origin, dst_extent)) =
            checked_region(dst, region.dst_subresource, region.dst_offset, region.extent) else { return };
        if extent != dst_extent { return; }
        encs.push(Enc::ResolveTexture {
            src: src.ir_id, src_sub, src_origin,
            dst: dst.ir_id, dst_sub, dst_origin, extent,
        });
        events.push(ImageEvent::TransferUse {
            image: src_image, range: src_range, required_layout: src_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_READ.as_raw(),
        });
        events.push(ImageEvent::TransferUse {
            image: dst_image, range: dst_range, required_layout: dst_image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_WRITE.as_raw(),
        });
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
        cb.image_events.extend(events);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdClearColorImage(
    command_buffer: VkCommandBuffer,
    image: VkImage,
    image_layout: vk::ImageLayout,
    p_color: *const vk::ClearColorValue,
    range_count: u32,
    p_ranges: *const vk::ImageSubresourceRange,
) {
    let (Some(color), false) = (unsafe { p_color.as_ref() }, p_ranges.is_null()) else {
        return;
    };
    if range_count == 0 || !transfer_layout_ok(image_layout, true) {
        return;
    }
    let ranges = unsafe { core::slice::from_raw_parts(p_ranges, range_count as usize) };
    let mut s = reg::lock();
    let Some(record) = s.images.get(&image) else {
        return;
    };
    if record.usage & vk::ImageUsageFlags::TRANSFER_DST.as_raw() == 0 || record.sample_count != 1 {
        return;
    }
    let mut encs = Vec::with_capacity(ranges.len());
    let mut events = Vec::with_capacity(ranges.len());
    for raw in ranges {
        let Some(range) = resolve_range(record, *raw) else {
            return;
        };
        if range.base_mip_level != 0
            || range.level_count != 1
            || range.base_array_layer != 0
            || range.layer_count != 1
        {
            return; // ClearRect currently addresses only the materialized base color view.
        }
        encs.push(Enc::ClearRect {
            texture: record.ir_id,
            x: 0,
            y: 0,
            w: record.width,
            h: record.height,
            color: unsafe { color.float32 },
        });
        events.push(ImageEvent::TransferUse {
            image,
            range,
            required_layout: image_layout.as_raw(),
            access: vk::AccessFlags::TRANSFER_WRITE.as_raw(),
        });
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
        cb.image_events.extend(events);
    }
}

// ---- submit + sync -------------------------------------------------------------------------------

fn for_each_subresource_mut(
    image: &mut reg::ImageRec,
    range: ImageSubresourceRange,
    mut visit: impl FnMut(&mut reg::ImageSubresourceState) -> bool,
) -> bool {
    for mip in range.base_mip_level..range.base_mip_level + range.level_count {
        for layer in range.base_array_layer..range.base_array_layer + range.layer_count {
            let Some(state) = image.subresources.get_mut(&(range.aspect_mask, mip, layer)) else {
                return false;
            };
            if !visit(state) {
                return false;
            }
        }
    }
    true
}

fn validate_image_events(
    images: &mut std::collections::HashMap<u64, reg::ImageRec>,
    events: &[ImageEvent],
) -> bool {
    for event in events {
        match event {
            ImageEvent::Barriers(barriers) => {
                // Each vkCmdPipelineBarrier call is one atomic transition group. Apply to a scratch
                // copy first so a mismatch in its tail cannot partially mutate its head.
                let mut trial = images.clone();
                for barrier in barriers {
                    let Some(image) = trial.get_mut(&barrier.image) else {
                        return false;
                    };
                    let old_undefined = barrier.old_layout == vk::ImageLayout::UNDEFINED.as_raw();
                    let explicit_ownership = barrier.src_queue_family != vk::QUEUE_FAMILY_IGNORED;
                    if !for_each_subresource_mut(image, barrier.range, |state| {
                        if (!old_undefined && state.layout != barrier.old_layout)
                            || (explicit_ownership && state.owner_queue_family != barrier.src_queue_family)
                        {
                            return false;
                        }
                        state.layout = barrier.new_layout;
                        state.last_access = barrier.dst_access;
                        state.last_stage = barrier.dst_stage;
                        if explicit_ownership {
                            state.owner_queue_family = barrier.dst_queue_family;
                        }
                        true
                    }) {
                        return false;
                    }
                }
                *images = trial;
            }
            ImageEvent::RenderBegin { image, range, initial_layout, subpass_layout } => {
                if *subpass_layout != vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw()
                    && *subpass_layout != vk::ImageLayout::GENERAL.as_raw()
                {
                    return false;
                }
                let Some(image) = images.get_mut(image) else {
                    return false;
                };
                let initial_undefined = *initial_layout == vk::ImageLayout::UNDEFINED.as_raw();
                if !for_each_subresource_mut(image, *range, |state| {
                    if !initial_undefined && state.layout != *initial_layout {
                        return false;
                    }
                    state.layout = *subpass_layout;
                    state.last_access = vk::AccessFlags::COLOR_ATTACHMENT_WRITE.as_raw();
                    state.last_stage = vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT.as_raw();
                    true
                }) {
                    return false;
                }
            }
            ImageEvent::RenderEnd { image, range, subpass_layout, final_layout } => {
                let Some(image) = images.get_mut(image) else {
                    return false;
                };
                if !for_each_subresource_mut(image, *range, |state| {
                    if state.layout != *subpass_layout {
                        return false;
                    }
                    state.layout = *final_layout;
                    true
                }) {
                    return false;
                }
            }
            ImageEvent::TransferUse { image, range, required_layout, access } => {
                let Some(image) = images.get_mut(image) else {
                    return false;
                };
                if !for_each_subresource_mut(image, *range, |state| {
                    if state.layout != *required_layout || state.owner_queue_family != 0 {
                        return false;
                    }
                    state.last_access = *access;
                    state.last_stage = vk::PipelineStageFlags::TRANSFER.as_raw();
                    true
                }) {
                    return false;
                }
            }
        }
    }
    true
}

/// `vkQueueSubmit` — the real submission state machine, ported from `MVKQueue::submit` +
/// `MVKQueueCommandBufferSubmission` (`GPUObjects/MVKQueue.mm`):
///  1. Validate the ENTIRE batch atomically before mutating anything: every command buffer must be
///     `Executable` (a one-time-submit buffer already executed is `Invalid` and rejected — MoltenVK
///     `canExecute()`); every wait semaphore must exist and be signaled.
///  2. Consume the wait semaphores (binary semaphores reset on wait), flush persistently-mapped
///     coherent memory, ship the recorded encoders as `Cmd::Submit`, and move each buffer to `Pending`.
///  3. On completion — synchronous here, since the host replays the shipped IR — signal the fence and
///     the signal semaphores, and retire each buffer (`Pending` → `Executable`, or `Invalid` if
///     one-time-submit). The fence-only submission form (`submitCount == 0 && fence`) just signals.
#[no_mangle]
pub extern "C" fn vkQueueSubmit(
    _queue: VkQueue,
    submit_count: u32,
    p_submits: *const vk::SubmitInfo,
    fence: VkFence,
) -> VkResult {
    let mut s = reg::lock();
    // Fence-only submission: nothing to execute, just signal the fence on (immediate) completion.
    if submit_count == 0 || p_submits.is_null() {
        if fence != 0 {
            if let Some(f) = s.fences.get_mut(&fence) {
                f.signaled = true;
            }
        }
        return VK_SUCCESS;
    }
    let submits = unsafe { core::slice::from_raw_parts(p_submits, submit_count as usize) };

    // ---- phase 1: validate the whole batch before any mutation (atomic accept/reject) ----
    let mut cb_keys: Vec<usize> = Vec::new();
    let mut wait_sems: Vec<u64> = Vec::new();
    let mut signal_sems: Vec<(u64, u64)> = Vec::new(); // (semaphore handle, timeline value; 0 = binary)
    for sub in submits {
        // Wait semaphores must all exist and be satisfied: a binary semaphore must be signaled; a timeline
        // semaphore's counter must already have reached the wait value (VK_KHR_timeline_semaphore — the
        // signaling submit/host-signal ran earlier in this synchronous model).
        if !sub.p_wait_semaphores.is_null() {
            let waits = unsafe {
                core::slice::from_raw_parts(sub.p_wait_semaphores, sub.wait_semaphore_count as usize)
            };
            let wait_values = crate::ext::timeline_wait_values(sub.p_next as *const core::ffi::c_void);
            for (i, &w) in waits.iter().enumerate() {
                let need = wait_values.get(i).copied().unwrap_or(0);
                match s.semaphores.get(&w.as_raw()) {
                    Some(sm) if sm.timeline && sm.counter >= need => {}
                    Some(sm) if !sm.timeline && sm.signaled => wait_sems.push(w.as_raw()),
                    _ => return VK_ERROR_INITIALIZATION_FAILED,
                }
            }
        }
        if !sub.p_signal_semaphores.is_null() {
            let sigs = unsafe {
                core::slice::from_raw_parts(sub.p_signal_semaphores, sub.signal_semaphore_count as usize)
            };
            // VK_KHR_timeline_semaphore: a `VkTimelineSemaphoreSubmitInfo` in the submit's pNext carries the
            // per-signal-semaphore counter values (aligned with pSignalSemaphores). Binary semaphores have
            // no value (recorded as 0 and applied as `signaled = true`).
            let values = crate::ext::timeline_signal_values(sub.p_next as *const core::ffi::c_void);
            for (i, &sg) in sigs.iter().enumerate() {
                signal_sems.push((sg.as_raw(), values.get(i).copied().unwrap_or(0)));
            }
        }
        if sub.p_command_buffers.is_null() {
            continue;
        }
        let cbs =
            unsafe { core::slice::from_raw_parts(sub.p_command_buffers, sub.command_buffer_count as usize) };
        for cb in cbs {
            let key = cb.as_raw() as usize;
            match s.cmdbufs.get(&key) {
                // Executable, and not a spent one-time-submit buffer (MoltenVK canExecute()).
                Some(rec)
                    if rec.state == CommandBufferState::Executable
                        && !(rec.one_time_submit && rec.was_executed) => {}
                _ => return VK_ERROR_INITIALIZATION_FAILED,
            }
            cb_keys.push(key);
        }
    }

    // Validate layout transitions and render attachment uses in command-buffer submission order on
    // a scratch image table. Recording alone never mutates global image state, and a rejected batch
    // leaves every command buffer executable and every subresource unchanged.
    let mut submitted_images = s.images.clone();
    for &key in &cb_keys {
        let Some(cb) = s.cmdbufs.get(&key) else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        if !validate_image_events(&mut submitted_images, &cb.image_events) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
    }

    // ---- phase 2: consume waits, ship the IR, move buffers Pending ----
    for w in &wait_sems {
        if let Some(sm) = s.semaphores.get_mut(w) {
            sm.signaled = false; // binary semaphore reset on wait
        }
    }
    // Flush persistently-mapped HOST_COHERENT buffers (vkcube writes its rotating MVP UBO every frame
    // without unmapping) so the host sees this frame's uniform data before the draw replays.
    s.flush_mapped();
    for (handle, submitted) in submitted_images {
        if let Some(image) = s.images.get_mut(&handle) {
            image.subresources = submitted.subresources;
        }
    }
    // The host Metal executor renders synchronously and does not model fence objects, so we never
    // signal a fence *in the shipped IR* — the fence/semaphore state machine is guest-side (below).
    // Per command buffer, emit its `vkCmdUpdateBuffer`/`vkCmdFillBuffer` uploads as `Cmd::WriteBuffer`
    // (start-of-submit, exactly like the persistently-mapped coherent flush above) and THEN its encoder
    // as `Cmd::Submit`. A command buffer with no such uploads (the vkcube draw/dispatch path) ships a
    // byte-identical `Cmd::Submit` stream — the deferred/query bookkeeping never enters the encoder.
    let mut shipped: Vec<(Vec<(u32, u64, Vec<u8>)>, Vec<Enc>)> = Vec::new();
    for &key in &cb_keys {
        if let Some(rec) = s.cmdbufs.get(&key) {
            shipped.push((rec.buffer_writes.clone(), rec.enc.clone()));
        }
    }
    for (writes, encoder) in shipped {
        for (id, offset, data) in writes {
            s.record(Cmd::WriteBuffer { id, offset, data });
        }
        s.record(Cmd::Submit(CommandBuffer { encoder, signal: None }));
    }
    for &key in &cb_keys {
        if let Some(cb) = s.cmdbufs.get_mut(&key) {
            cb.state = CommandBufferState::Pending;
            cb.was_executed = true;
        }
    }

    // ---- phase 3: completion (synchronous) — retire buffers, signal semaphores + fence ----
    for &key in &cb_keys {
        if let Some(cb) = s.cmdbufs.get_mut(&key) {
            cb.state = if cb.one_time_submit {
                CommandBufferState::Invalid
            } else {
                CommandBufferState::Executable
            };
        }
    }
    for (sg, value) in &signal_sems {
        if let Some(sm) = s.semaphores.get_mut(sg) {
            if sm.timeline {
                sm.counter = sm.counter.max(*value); // timeline signal advances the counter monotonically
            } else {
                sm.signaled = true;
            }
        }
    }
    if fence != 0 {
        if let Some(f) = s.fences.get_mut(&fence) {
            f.signaled = true;
        }
    }
    // Apply the recorded device query/event ops on (synchronous) completion, in submission order.
    let deferred: Vec<reg::DeferredOp> =
        cb_keys.iter().filter_map(|k| s.cmdbufs.get(k)).flat_map(|cb| cb.deferred.clone()).collect();
    for op in deferred {
        crate::query::apply_deferred(&mut s, op);
    }
    VK_SUCCESS
}

/// `vkQueueWaitIdle` — wait for all submitted work on the queue to complete. Our host replay is
/// synchronous, so any `Pending` command buffer has already completed by the time control returns;
/// retire them (`Pending` → `Executable`/`Invalid`) and report success. Ported from `MVKQueue::waitIdle`.
#[no_mangle]
pub extern "C" fn vkQueueWaitIdle(queue: VkQueue) -> VkResult {
    let _ = queue;
    let mut s = reg::lock();
    for cb in s.cmdbufs.values_mut() {
        if cb.state == CommandBufferState::Pending {
            cb.state = if cb.one_time_submit {
                CommandBufferState::Invalid
            } else {
                CommandBufferState::Executable
            };
        }
    }
    VK_SUCCESS
}

/// `vkDeviceWaitIdle` — wait for all queues. Delegates to the same synchronous drain as the queue.
#[no_mangle]
pub extern "C" fn vkDeviceWaitIdle(_device: VkDevice) -> VkResult {
    vkQueueWaitIdle(core::ptr::null_mut())
}

// ---- fences + semaphores -------------------------------------------------------------------------

/// `vkCreateFence` — a guest-side fence, unsignaled unless `VK_FENCE_CREATE_SIGNALED_BIT` is set
/// (MoltenVK `MVKFence`). The host Metal executor renders synchronously and doesn't model fences, so we
/// don't emit `Cmd::CreateFence`; the fence's signaled state is a guest-side machine (see `vkQueueSubmit`).
#[no_mangle]
pub extern "C" fn vkCreateFence(
    _device: VkDevice,
    p_create_info: *const vk::FenceCreateInfo,
    _p_allocator: *const c_void,
    p_fence: *mut VkFence,
) -> VkResult {
    let Some(out) = (unsafe { p_fence.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let signaled = unsafe { p_create_info.as_ref() }
        .map(|ci| ci.flags.contains(vk::FenceCreateFlags::SIGNALED))
        .unwrap_or(false);
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    s.fences.insert(handle, reg::FenceRec { ir_id, signaled });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyFence(_device: VkDevice, fence: VkFence, _p_allocator: *const c_void) {
    reg::lock().fences.remove(&fence);
}

/// `vkWaitForFences` — wait for the requested fences. Because submission completion is synchronous, a
/// fence guarding completed work is already signaled. Honestly report `VK_TIMEOUT` when the
/// (`wait_all` ? all : any) requested fences are NOT signaled and the caller asked not to block past
/// `timeout == 0`; otherwise success. Never blocks (the work is already done or was never submitted).
#[no_mangle]
pub extern "C" fn vkWaitForFences(
    _device: VkDevice,
    fence_count: u32,
    p_fences: *const VkFence,
    wait_all: u32,
    timeout: u64,
) -> VkResult {
    if p_fences.is_null() || fence_count == 0 {
        return VK_SUCCESS;
    }
    let fences = unsafe { core::slice::from_raw_parts(p_fences, fence_count as usize) };
    let s = reg::lock();
    let is_signaled = |h: &VkFence| s.fences.get(h).map(|f| f.signaled).unwrap_or(false);
    let satisfied = if wait_all != 0 {
        fences.iter().all(is_signaled)
    } else {
        fences.iter().any(is_signaled)
    };
    if satisfied {
        VK_SUCCESS
    } else if timeout == 0 {
        VK_TIMEOUT
    } else {
        // Synchronous model: any submitted work has already completed. An unsignaled fence here means
        // no submission signals it, so waiting can never succeed — report the timeout truthfully.
        VK_TIMEOUT
    }
}

/// `vkResetFences` — return the fences to the unsignaled state.
#[no_mangle]
pub extern "C" fn vkResetFences(_device: VkDevice, fence_count: u32, p_fences: *const VkFence) -> VkResult {
    if p_fences.is_null() {
        return VK_SUCCESS;
    }
    let fences = unsafe { core::slice::from_raw_parts(p_fences, fence_count as usize) };
    let mut s = reg::lock();
    for h in fences {
        if let Some(f) = s.fences.get_mut(h) {
            f.signaled = false;
        }
    }
    VK_SUCCESS
}

/// `vkGetFenceStatus` — `VK_SUCCESS` if the fence is signaled, `VK_NOT_READY` if not (spec §7.3).
#[no_mangle]
pub extern "C" fn vkGetFenceStatus(_device: VkDevice, fence: VkFence) -> VkResult {
    match reg::lock().fences.get(&fence) {
        Some(f) if f.signaled => VK_SUCCESS,
        Some(_) => VK_NOT_READY,
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// `vkCreateSemaphore` — a guest-side binary semaphore, unsignaled at creation (MoltenVK `MVKSemaphore`).
/// Its signaled state is driven by `vkQueueSubmit` waits/signals (and WSI acquire in a later increment).
#[no_mangle]
pub extern "C" fn vkCreateSemaphore(
    _device: VkDevice,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_semaphore: *mut VkSemaphore,
) -> VkResult {
    let Some(out) = (unsafe { p_semaphore.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // VK_KHR_timeline_semaphore: a `VkSemaphoreTypeCreateInfo` in the pNext chain selects a timeline
    // semaphore + its initial counter value (else a binary semaphore). Ported from `MVKSemaphore` /
    // `MVKTimelineSemaphore`'s type dispatch.
    let (timeline, initial) = crate::ext::parse_semaphore_type(p_create_info);
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.semaphores.insert(
        handle,
        reg::SemaphoreRec { signaled: false, timeline, counter: if timeline { initial } else { 0 } },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySemaphore(_device: VkDevice, semaphore: VkSemaphore, _p_allocator: *const c_void) {
    reg::lock().semaphores.remove(&semaphore);
}

// ---- dynamic pipeline state (MVKCommandEncoderState) ---------------------------------------------
//
// The IR pipeline is largely static; these commands record the dynamic state verbatim into the
// command buffer (observable), matching MoltenVK's `MVKCommandEncoderState`. Lowering the recorded
// state into the IR draw parameters is a later increment (they are classified `partial`).

#[no_mangle]
pub extern "C" fn vkCmdSetLineWidth(command_buffer: VkCommandBuffer, line_width: f32) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        cb.dynamic.line_width = line_width;
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBias(
    command_buffer: VkCommandBuffer,
    constant_factor: f32,
    clamp: f32,
    slope_factor: f32,
) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        cb.dynamic.depth_bias = (constant_factor, clamp, slope_factor);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBounds(command_buffer: VkCommandBuffer, min_depth_bounds: f32, max_depth_bounds: f32) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        cb.dynamic.depth_bounds = (min_depth_bounds, max_depth_bounds);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetBlendConstants(command_buffer: VkCommandBuffer, blend_constants: *const f32) {
    if blend_constants.is_null() {
        return;
    }
    let c = unsafe { core::slice::from_raw_parts(blend_constants, 4) };
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        cb.dynamic.blend_constants = [c[0], c[1], c[2], c[3]];
    }
}

/// VkStencilFaceFlags: FRONT = 0x1, BACK = 0x2, FRONT_AND_BACK = 0x3. Apply `value` to the selected faces.
fn set_stencil_faces(pair: &mut (u32, u32), face_mask: u32, value: u32) {
    if face_mask & 0x1 != 0 {
        pair.0 = value;
    }
    if face_mask & 0x2 != 0 {
        pair.1 = value;
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilCompareMask(command_buffer: VkCommandBuffer, face_mask: vk::StencilFaceFlags, compare_mask: u32) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        set_stencil_faces(&mut cb.dynamic.stencil_compare_mask, face_mask.as_raw(), compare_mask);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilWriteMask(command_buffer: VkCommandBuffer, face_mask: vk::StencilFaceFlags, write_mask: u32) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        set_stencil_faces(&mut cb.dynamic.stencil_write_mask, face_mask.as_raw(), write_mask);
    }
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilReference(command_buffer: VkCommandBuffer, face_mask: vk::StencilFaceFlags, reference: u32) {
    if let Some(cb) = reg::lock().recording_mut(cb_key(command_buffer)) {
        set_stencil_faces(&mut cb.dynamic.stencil_reference, face_mask.as_raw(), reference);
    }
}

// ---- push constants ------------------------------------------------------------------------------

/// `vkCmdPushConstants` — write `size` bytes at `offset` into the command buffer's push-constant block,
/// validated against the pipeline layout's declared ranges (a matching stage + covering range must
/// exist). Retained guest-side (MoltenVK `MVKCommandPushConstants`); the IR does not yet carry a
/// push-constant block, so this is a bounded (`partial`) recording.
#[no_mangle]
pub extern "C" fn vkCmdPushConstants(
    command_buffer: VkCommandBuffer,
    layout: VkPipelineLayout,
    stage_flags: vk::ShaderStageFlags,
    offset: u32,
    size: u32,
    p_values: *const c_void,
) {
    if p_values.is_null() || size == 0 || offset % 4 != 0 || size % 4 != 0 {
        return;
    }
    let stages = stage_flags.as_raw();
    let mut s = reg::lock();
    // A declared range with intersecting stages must fully cover [offset, offset+size).
    let covered = s.pipeline_layouts.get(&layout).is_some_and(|l| {
        l.push_ranges.iter().any(|(rstages, roff, rsize)| {
            rstages & stages != 0
                && offset >= *roff
                && offset.checked_add(size).is_some_and(|end| end <= roff.saturating_add(*rsize))
        })
    });
    if !covered {
        return;
    }
    let bytes = unsafe { core::slice::from_raw_parts(p_values as *const u8, size as usize) };
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        let end = offset as usize + size as usize;
        if cb.push_constants.len() < end {
            cb.push_constants.resize(end, 0);
        }
        cb.push_constants[offset as usize..end].copy_from_slice(bytes);
    }
}

// ---- buffer fill / inline update -----------------------------------------------------------------

/// `vkCmdUpdateBuffer` — record a small (≤65536-byte, 4-aligned) inline upload into `dstBuffer`,
/// emitted as an IR `WriteBuffer` at the start of the owning submit (the same model as a
/// persistently-mapped coherent flush). Ported from `MVKCmdBufferUpdate`.
#[no_mangle]
pub extern "C" fn vkCmdUpdateBuffer(
    command_buffer: VkCommandBuffer,
    dst_buffer: VkBuffer,
    dst_offset: u64,
    data_size: u64,
    p_data: *const c_void,
) {
    if p_data.is_null() || data_size == 0 || data_size > 65536 || data_size % 4 != 0 || dst_offset % 4 != 0 {
        return;
    }
    let mut s = reg::lock();
    let Some(b) = s.buffers.get(&dst_buffer) else { return };
    if b.usage & buffer_usage::COPY_DST == 0 {
        return;
    }
    match dst_offset.checked_add(data_size) {
        Some(end) if end <= b.size => {}
        _ => return,
    }
    let ir_id = b.ir_id;
    let bytes = unsafe { core::slice::from_raw_parts(p_data as *const u8, data_size as usize) }.to_vec();
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.buffer_writes.push((ir_id, dst_offset, bytes));
    }
}

/// `vkCmdFillBuffer` — fill `[dstOffset, dstOffset+size)` of `dstBuffer` with the 32-bit `data` value,
/// emitted as an IR `WriteBuffer` at the start of the owning submit. `VK_WHOLE_SIZE` fills to the end.
/// Ported from `MVKCmdFillBuffer`.
#[no_mangle]
pub extern "C" fn vkCmdFillBuffer(
    command_buffer: VkCommandBuffer,
    dst_buffer: VkBuffer,
    dst_offset: u64,
    size: u64,
    data: u32,
) {
    if dst_offset % 4 != 0 {
        return;
    }
    let mut s = reg::lock();
    let Some(b) = s.buffers.get(&dst_buffer) else { return };
    if b.usage & buffer_usage::COPY_DST == 0 {
        return;
    }
    let fill_size = if size == u64::MAX { b.size.saturating_sub(dst_offset) } else { size };
    if fill_size == 0 || fill_size % 4 != 0 {
        return;
    }
    match dst_offset.checked_add(fill_size) {
        Some(end) if end <= b.size => {}
        _ => return,
    }
    let ir_id = b.ir_id;
    let words = (fill_size / 4) as usize;
    let mut bytes = Vec::with_capacity(words * 4);
    let le = data.to_le_bytes();
    for _ in 0..words {
        bytes.extend_from_slice(&le);
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.buffer_writes.push((ir_id, dst_offset, bytes));
    }
}

// ---- clears within / of a render target ----------------------------------------------------------

/// `vkCmdClearAttachments` — clear regions of the bound render-pass color attachment. Lowered to
/// `Enc::ClearRect` for each color attachment × rect inside the active render pass; depth/stencil
/// attachment clears are not materialized (bounded → `partial`). Ported from `MVKCmdClearAttachments`.
#[no_mangle]
pub extern "C" fn vkCmdClearAttachments(
    command_buffer: VkCommandBuffer,
    attachment_count: u32,
    p_attachments: *const vk::ClearAttachment,
    rect_count: u32,
    p_rects: *const vk::ClearRect,
) {
    if attachment_count == 0 || rect_count == 0 || p_attachments.is_null() || p_rects.is_null() {
        return;
    }
    let attachments = unsafe { core::slice::from_raw_parts(p_attachments, attachment_count as usize) };
    let rects = unsafe { core::slice::from_raw_parts(p_rects, rect_count as usize) };
    let mut s = reg::lock();
    // Resolve the active render target's IR texture (must be inside a render pass).
    let Some((image, _, _, _)) = s.cmdbufs.get(&cb_key(command_buffer)).and_then(|cb| {
        (cb.state == CommandBufferState::Recording && cb.in_render_pass)
            .then_some(cb.active_render_image)
            .flatten()
    }) else {
        return;
    };
    let Some(tex) = s.images.get(&image).map(|i| i.ir_id) else { return };
    let mut encs = Vec::new();
    for att in attachments {
        if att.aspect_mask & vk::ImageAspectFlags::COLOR != vk::ImageAspectFlags::COLOR {
            continue; // depth/stencil clears are not materialized in this bring-up model
        }
        let color = unsafe { att.clear_value.color.float32 };
        for r in rects {
            if r.layer_count == 0 || r.rect.extent.width == 0 || r.rect.extent.height == 0 {
                continue;
            }
            encs.push(Enc::ClearRect {
                texture: tex,
                x: r.rect.offset.x.max(0) as u32,
                y: r.rect.offset.y.max(0) as u32,
                w: r.rect.extent.width,
                h: r.rect.extent.height,
                color,
            });
        }
    }
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        cb.enc.extend(encs);
    }
}

/// `vkCmdClearDepthStencilImage` — validate a depth/stencil clear. Depth/stencil textures are not
/// materialized by the software oracle and the IR has no depth-clear op, so this validates the target
/// (image exists, TRANSFER_DST, depth/stencil aspect, supported layout) and records no color op
/// (bounded → `partial`). Ported from `MVKCmdClearDepthStencilImage`.
#[no_mangle]
pub extern "C" fn vkCmdClearDepthStencilImage(
    command_buffer: VkCommandBuffer,
    image: VkImage,
    image_layout: vk::ImageLayout,
    _p_depth_stencil: *const vk::ClearDepthStencilValue,
    range_count: u32,
    p_ranges: *const vk::ImageSubresourceRange,
) {
    if range_count == 0 || p_ranges.is_null() || !transfer_layout_ok(image_layout, true) {
        return;
    }
    let s = reg::lock();
    // Must be a known image being cleared as a transfer destination while recording.
    let known = s.images.get(&image).is_some_and(|i| i.usage & vk::ImageUsageFlags::TRANSFER_DST.as_raw() != 0);
    let recording = s.cmdbufs.get(&cb_key(command_buffer)).is_some_and(|cb| cb.state == CommandBufferState::Recording);
    let _ = (known, recording);
}

// ---- subpass + secondary command buffers ---------------------------------------------------------

/// `vkCmdNextSubpass` — advance to the next subpass. Our render-pass model is single-subpass, so this
/// is validated (must be inside a render pass) and is a no-op advance (bounded → `partial`). Ported
/// from `MVKCmdNextSubpass`.
#[no_mangle]
pub extern "C" fn vkCmdNextSubpass(command_buffer: VkCommandBuffer, _contents: i32) {
    let _ = reg::lock()
        .cmdbufs
        .get(&cb_key(command_buffer))
        .map(|cb| cb.state == CommandBufferState::Recording && cb.in_render_pass);
}

/// `vkCmdExecuteCommands` — replay recorded secondary command buffers into this primary: their encoder
/// ops, image events, deferred query/event ops and inline buffer writes are appended in order. Each
/// secondary must be `Executable`. Ported from `MVKCmdExecuteCommands`.
#[no_mangle]
pub extern "C" fn vkCmdExecuteCommands(
    command_buffer: VkCommandBuffer,
    command_buffer_count: u32,
    p_command_buffers: *const VkCommandBuffer,
) {
    if command_buffer_count == 0 || p_command_buffers.is_null() {
        return;
    }
    let secondaries = unsafe { core::slice::from_raw_parts(p_command_buffers, command_buffer_count as usize) };
    let mut s = reg::lock();
    // The primary must be recording; every secondary must exist and be Executable (validate first).
    if s.recording_mut(cb_key(command_buffer)).is_none() {
        return;
    }
    let keys: Vec<usize> = secondaries.iter().map(|&c| c as usize).collect();
    if !keys.iter().all(|k| s.cmdbufs.get(k).is_some_and(|c| c.state == CommandBufferState::Executable)) {
        return;
    }
    // Snapshot each secondary's recorded work, then splice it into the primary in order.
    let mut spliced: Vec<(Vec<Enc>, Vec<ImageEvent>, Vec<reg::DeferredOp>, Vec<(u32, u64, Vec<u8>)>)> =
        Vec::new();
    for k in &keys {
        if let Some(sec) = s.cmdbufs.get(k) {
            spliced.push((sec.enc.clone(), sec.image_events.clone(), sec.deferred.clone(), sec.buffer_writes.clone()));
        }
    }
    if let Some(primary) = s.recording_mut(cb_key(command_buffer)) {
        for (enc, events, deferred, writes) in spliced {
            primary.enc.extend(enc);
            primary.image_events.extend(events);
            primary.deferred.extend(deferred);
            primary.buffer_writes.extend(writes);
        }
    }
}

// ---- indirect draw / dispatch --------------------------------------------------------------------

/// Validate an indirect-parameter buffer read: it must exist, carry INDIRECT usage, and the read span
/// `[offset, offset + max(1,count-1)*stride + struct_size)` must fit. The IR has no indirect encoder
/// op, so a validated indirect command records no draw (bounded → `partial`).
fn indirect_ok(s: &reg::VkState, buffer: VkBuffer, offset: u64, draw_count: u32, stride: u32, struct_size: u64) -> bool {
    let Some(b) = s.buffers.get(&buffer) else { return false };
    if b.usage & buffer_usage::INDIRECT == 0 || draw_count == 0 {
        return false;
    }
    let last = (draw_count as u64 - 1).saturating_mul(stride as u64);
    last.checked_add(struct_size)
        .and_then(|span| offset.checked_add(span))
        .is_some_and(|end| end <= b.size)
}

#[no_mangle]
pub extern "C" fn vkCmdDrawIndirect(
    command_buffer: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
) {
    let _ = command_buffer;
    let s = reg::lock();
    // VkDrawIndirectCommand is 16 bytes.
    let _ok = indirect_ok(&s, buffer, offset, draw_count, stride, 16);
}

#[no_mangle]
pub extern "C" fn vkCmdDrawIndexedIndirect(
    command_buffer: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    draw_count: u32,
    stride: u32,
) {
    let _ = command_buffer;
    let s = reg::lock();
    // VkDrawIndexedIndirectCommand is 20 bytes.
    let _ok = indirect_ok(&s, buffer, offset, draw_count, stride, 20);
}

#[no_mangle]
pub extern "C" fn vkCmdDispatchIndirect(command_buffer: VkCommandBuffer, buffer: VkBuffer, offset: u64) {
    let _ = command_buffer;
    let s = reg::lock();
    // VkDispatchIndirectCommand is 12 bytes.
    let _ok = indirect_ok(&s, buffer, offset, 1, 0, 12);
}

// ---- sparse binding ------------------------------------------------------------------------------

/// `vkQueueBindSparse` — we advertise no sparse residency, so there are no sparse resources to (un)bind;
/// this handles the accompanying binary-semaphore + fence synchronization exactly like a queue submit
/// (waits consumed, signals + fence signalled on synchronous completion) and treats the bind lists as
/// empty (bounded → `partial`). Ported from `MVKQueue::bindSparse`.
#[no_mangle]
pub extern "C" fn vkQueueBindSparse(
    _queue: VkQueue,
    bind_info_count: u32,
    p_bind_info: *const vk::BindSparseInfo,
    fence: VkFence,
) -> VkResult {
    let mut s = reg::lock();
    if bind_info_count != 0 && !p_bind_info.is_null() {
        let infos = unsafe { core::slice::from_raw_parts(p_bind_info, bind_info_count as usize) };
        // Phase 1: every wait semaphore must exist and be signaled (atomic accept/reject).
        let mut waits: Vec<u64> = Vec::new();
        let mut signals: Vec<u64> = Vec::new();
        for info in infos {
            if !info.p_wait_semaphores.is_null() {
                let w = unsafe { core::slice::from_raw_parts(info.p_wait_semaphores, info.wait_semaphore_count as usize) };
                for sem in w {
                    match s.semaphores.get(&sem.as_raw()) {
                        Some(sm) if sm.signaled => waits.push(sem.as_raw()),
                        _ => return VK_ERROR_INITIALIZATION_FAILED,
                    }
                }
            }
            if !info.p_signal_semaphores.is_null() {
                let sg = unsafe { core::slice::from_raw_parts(info.p_signal_semaphores, info.signal_semaphore_count as usize) };
                signals.extend(sg.iter().map(|x| x.as_raw()));
            }
        }
        for w in &waits {
            if let Some(sm) = s.semaphores.get_mut(w) {
                sm.signaled = false;
            }
        }
        for sg in &signals {
            if let Some(sm) = s.semaphores.get_mut(sg) {
                sm.signaled = true;
            }
        }
    }
    if fence != 0 {
        if let Some(f) = s.fences.get_mut(&fence) {
            f.signaled = true;
        }
    }
    VK_SUCCESS
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    use crate::reg::TEST_SERIAL as TEST_LOCK; // shared crate-wide test lock (serializes ir_log writers)

    fn create_image() -> VkImage {
        let ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: 8, height: 8, depth: 1 })
            .mip_levels(2)
            .array_layers(2)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let mut image = 0;
        assert_eq!(
            crate::memory::vkCreateImage(core::ptr::null_mut(), &ci, core::ptr::null(), &mut image),
            VK_SUCCESS
        );
        image
    }

    fn create_image_samples(samples: vk::SampleCountFlags) -> VkImage {
        let ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: 8, height: 8, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let mut image = 0;
        assert_eq!(
            crate::memory::vkCreateImage(core::ptr::null_mut(), &ci, core::ptr::null(), &mut image),
            VK_SUCCESS
        );
        image
    }

    #[test]
    fn resolve_is_distinct_and_invalid_region_batch_does_not_mutate_recording() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let src = create_image_samples(vk::SampleCountFlags::TYPE_4);
        let dst = create_image_samples(vk::SampleCountFlags::TYPE_1);
        let cb = 0x1212usize as VkCommandBuffer;
        begin(cb);
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        let good = vk::ImageResolve {
            src_subresource: layers,
            src_offset: vk::Offset3D { x: 1, y: 2, z: 0 },
            dst_subresource: layers,
            dst_offset: vk::Offset3D { x: 3, y: 4, z: 0 },
            extent: vk::Extent3D { width: 2, height: 2, depth: 1 },
        };
        vkCmdResolveImage(
            cb, src, vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            dst, vk::ImageLayout::TRANSFER_DST_OPTIMAL, 1, &good,
        );
        let key = cb as usize;
        let before = reg::lock().cmdbufs[&key].enc.clone();
        assert!(matches!(before.last(), Some(Enc::ResolveTexture {
            src_origin: Origin3d { x: 1, y: 2, z: 0 },
            dst_origin: Origin3d { x: 3, y: 4, z: 0 },
            extent: Extent3d { width: 2, height: 2, depth: 1 }, ..
        })));

        let mut bad = good;
        bad.src_subresource.layer_count = 2;
        let batch = [good, bad];
        vkCmdResolveImage(
            cb, src, vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            dst, vk::ImageLayout::TRANSFER_DST_OPTIMAL, 2, batch.as_ptr(),
        );
        assert_eq!(reg::lock().cmdbufs[&key].enc, before, "invalid batch appended no partial resolve");
    }

    fn begin(cb: VkCommandBuffer) {
        let info = vk::CommandBufferBeginInfo::default();
        assert_eq!(vkBeginCommandBuffer(cb, &info), VK_SUCCESS);
    }

    fn submit(cb: VkCommandBuffer) -> VkResult {
        let command = vk::CommandBuffer::from_raw(cb as usize as u64);
        let info = vk::SubmitInfo {
            command_buffer_count: 1,
            p_command_buffers: &command,
            ..Default::default()
        };
        vkQueueSubmit(core::ptr::null_mut(), 1, &info, 0)
    }

    fn barrier(
        cb: VkCommandBuffer,
        image: VkImage,
        old: vk::ImageLayout,
        new: vk::ImageLayout,
        base_mip: u32,
        level_count: u32,
        base_layer: u32,
        layer_count: u32,
    ) {
        let b = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .old_layout(old)
            .new_layout(new)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(vk::Image::from_raw(image))
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: base_mip,
                level_count,
                base_array_layer: base_layer,
                layer_count,
            });
        vkCmdPipelineBarrier(
            cb,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::DependencyFlags::empty(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            1,
            &b,
        );
    }

    fn layout(image: VkImage, mip: u32, layer: u32) -> i32 {
        reg::lock().images[&image].subresources[&(vk::ImageAspectFlags::COLOR.as_raw(), mip, layer)].layout
    }

    fn create_render_objects(image: VkImage, initial: vk::ImageLayout) -> (VkImageView, VkRenderPass, VkFramebuffer) {
        let view_ci = vk::ImageViewCreateInfo::default()
            .image(vk::Image::from_raw(image))
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let mut view = 0;
        assert_eq!(
            crate::memory::vkCreateImageView(core::ptr::null_mut(), &view_ci, core::ptr::null(), &mut view),
            VK_SUCCESS
        );
        let attachment = vk::AttachmentDescription::default()
            .format(vk::Format::R8G8B8A8_UNORM)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(initial)
            .final_layout(vk::ImageLayout::GENERAL);
        let color_ref = vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        };
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(core::slice::from_ref(&color_ref));
        let rp_ci = vk::RenderPassCreateInfo::default()
            .attachments(core::slice::from_ref(&attachment))
            .subpasses(core::slice::from_ref(&subpass));
        let mut render_pass = 0;
        assert_eq!(
            crate::pipeline::vkCreateRenderPass(core::ptr::null_mut(), &rp_ci, core::ptr::null(), &mut render_pass),
            VK_SUCCESS
        );
        let attachment_view = vk::ImageView::from_raw(view);
        let fb_ci = vk::FramebufferCreateInfo::default()
            .render_pass(vk::RenderPass::from_raw(render_pass))
            .attachments(core::slice::from_ref(&attachment_view))
            .width(8)
            .height(8)
            .layers(1);
        let mut framebuffer = 0;
        assert_eq!(
            crate::pipeline::vkCreateFramebuffer(core::ptr::null_mut(), &fb_ci, core::ptr::null(), &mut framebuffer),
            VK_SUCCESS
        );
        (view, render_pass, framebuffer)
    }

    #[test]
    fn image_layout_barriers_track_subresources_and_apply_atomically_at_submit() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let image = create_image();
        let cb = 0x1010usize as VkCommandBuffer;
        begin(cb);
        barrier(
            cb,
            image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            0,
            1,
            0,
            1,
        );
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::UNDEFINED.as_raw(), "recording must not apply state");
        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_SUCCESS);
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::TRANSFER_DST_OPTIMAL.as_raw());
        assert_eq!(layout(image, 1, 0), vk::ImageLayout::UNDEFINED.as_raw());
        assert_eq!(layout(image, 0, 1), vk::ImageLayout::UNDEFINED.as_raw());
        let state = reg::lock().images[&image].subresources
            [&(vk::ImageAspectFlags::COLOR.as_raw(), 0, 0)];
        assert_eq!(state.last_access, vk::AccessFlags::COLOR_ATTACHMENT_WRITE.as_raw());
        assert_eq!(state.last_stage, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT.as_raw());
        assert_eq!(state.owner_queue_family, 0);

        // A stale oldLayout fails at submit and leaves both image and executable command buffer intact.
        begin(cb);
        barrier(
            cb,
            image,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::GENERAL,
            0,
            1,
            0,
            1,
        );
        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_ERROR_INITIALIZATION_FAILED);
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::TRANSFER_DST_OPTIMAL.as_raw());
        assert_eq!(reg::lock().cmdbufs[&(cb as usize)].state, CommandBufferState::Executable);

        // One bad tail transition rolls back the valid head of the same barrier call.
        begin(cb);
        let first = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(vk::Image::from_raw(image))
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let second = vk::ImageMemoryBarrier {
            old_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            new_layout: vk::ImageLayout::GENERAL,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 1,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
            ..first
        };
        let barriers = [first, second];
        vkCmdPipelineBarrier(
            cb,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            2,
            barriers.as_ptr(),
        );
        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_ERROR_INITIALIZATION_FAILED);
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::TRANSFER_DST_OPTIMAL.as_raw());

        // A structurally invalid range records nothing; a following valid barrier in the same command
        // buffer still submits, proving the invalid call did not poison or partially mutate recording.
        begin(cb);
        barrier(
            cb,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::GENERAL,
            99,
            1,
            0,
            1,
        );
        barrier(
            cb,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            0,
            1,
            0,
            1,
        );
        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_SUCCESS);
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw());

        // Render-pass implicit transitions participate in the same submit-time validation. A stale
        // declared initial layout rejects the pass without changing the attachment; the matching
        // declaration then transitions COLOR_ATTACHMENT_OPTIMAL -> GENERAL on successful completion.
        let (bad_view, bad_rp, bad_fb) =
            create_render_objects(image, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        begin(cb);
        let begin_info = vk::RenderPassBeginInfo::default()
            .render_pass(vk::RenderPass::from_raw(bad_rp))
            .framebuffer(vk::Framebuffer::from_raw(bad_fb))
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D { width: 8, height: 8 },
            });
        vkCmdBeginRenderPass(cb, &begin_info, vk::SubpassContents::INLINE.as_raw());
        vkCmdEndRenderPass(cb);
        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_ERROR_INITIALIZATION_FAILED);
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw());
        crate::pipeline::vkDestroyFramebuffer(core::ptr::null_mut(), bad_fb, core::ptr::null());
        crate::pipeline::vkDestroyRenderPass(core::ptr::null_mut(), bad_rp, core::ptr::null());
        crate::memory::vkDestroyImageView(core::ptr::null_mut(), bad_view, core::ptr::null());

        let (view, render_pass, framebuffer) =
            create_render_objects(image, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        begin(cb);
        let begin_info = vk::RenderPassBeginInfo::default()
            .render_pass(vk::RenderPass::from_raw(render_pass))
            .framebuffer(vk::Framebuffer::from_raw(framebuffer))
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D { width: 8, height: 8 },
            });
        vkCmdBeginRenderPass(cb, &begin_info, vk::SubpassContents::INLINE.as_raw());
        vkCmdEndRenderPass(cb);
        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_SUCCESS);
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::GENERAL.as_raw());
        crate::pipeline::vkDestroyFramebuffer(core::ptr::null_mut(), framebuffer, core::ptr::null());
        crate::pipeline::vkDestroyRenderPass(core::ptr::null_mut(), render_pass, core::ptr::null());
        crate::memory::vkDestroyImageView(core::ptr::null_mut(), view, core::ptr::null());
        crate::memory::vkDestroyImage(core::ptr::null_mut(), image, core::ptr::null());
    }

    #[test]
    fn sync2_pipeline_barrier2_tracks_subresources_and_ownership_like_the_legacy_form() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let image = create_image(); // 2 mips x 2 layers, UNDEFINED
        let cb = 0x3131usize as VkCommandBuffer;

        // A sync2 barrier on mip 1 / layer 1 only: 64-bit stage+access masks carried inline per barrier.
        let barrier2 = |old: vk::ImageLayout, new: vk::ImageLayout, src_qf: u32, dst_qf: u32| {
            let b = vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .old_layout(old)
                .new_layout(new)
                .src_queue_family_index(src_qf)
                .dst_queue_family_index(dst_qf)
                .image(vk::Image::from_raw(image))
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 1,
                    level_count: 1,
                    base_array_layer: 1,
                    layer_count: 1,
                });
            let barriers = [b];
            let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
            begin(cb);
            vkCmdPipelineBarrier2(cb, &dep);
            assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
            submit(cb)
        };

        // Recording alone must not mutate global state; the transition applies only at submit, and only
        // to the addressed subresource (mip 1 / layer 1), leaving the others UNDEFINED.
        assert_eq!(
            barrier2(vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED),
            VK_SUCCESS
        );
        assert_eq!(layout(image, 1, 1), vk::ImageLayout::TRANSFER_DST_OPTIMAL.as_raw());
        assert_eq!(layout(image, 0, 0), vk::ImageLayout::UNDEFINED.as_raw());
        assert_eq!(layout(image, 1, 0), vk::ImageLayout::UNDEFINED.as_raw());

        // Explicit single-family (0 -> 0) ownership transfer is accepted and tracked; a stale oldLayout
        // is rejected atomically at submit without changing the subresource.
        assert_eq!(
            barrier2(vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::GENERAL, 0, 0),
            VK_SUCCESS
        );
        assert_eq!(layout(image, 1, 1), vk::ImageLayout::GENERAL.as_raw());
        assert_eq!(
            barrier2(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED),
            VK_ERROR_INITIALIZATION_FAILED
        );
        assert_eq!(layout(image, 1, 1), vk::ImageLayout::GENERAL.as_raw(), "rejected sync2 barrier left state intact");

        crate::memory::vkDestroyImage(core::ptr::null_mut(), image, core::ptr::null());
    }

    #[test]
    fn vulkan_transfer_regions_lower_without_field_loss_and_reject_atomically() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let src = create_image();
        let dst = create_image();
        let cb = 0x2020usize as VkCommandBuffer;

        let buffer_ci = vk::BufferCreateInfo::default()
            .size(512)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST);
        let mut upload = 0;
        let mut readback = 0;
        assert_eq!(
            crate::memory::vkCreateBuffer(core::ptr::null_mut(), &buffer_ci, core::ptr::null(), &mut upload),
            VK_SUCCESS
        );
        assert_eq!(
            crate::memory::vkCreateBuffer(core::ptr::null_mut(), &buffer_ci, core::ptr::null(), &mut readback),
            VK_SUCCESS
        );

        begin(cb);
        barrier(cb, src, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, 0, 1, 0, 1);
        barrier(cb, dst, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, 0, 1, 0, 1);
        let buffer_region = vk::BufferImageCopy {
            buffer_offset: 16,
            buffer_row_length: 8,
            buffer_image_height: 5,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
            image_extent: vk::Extent3D { width: 4, height: 3, depth: 1 },
        };
        vkCmdCopyBufferToImage(
            cb,
            upload,
            src,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            1,
            &buffer_region,
        );
        barrier(
            cb,
            src,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            0,
            1,
            0,
            1,
        );
        let image_region = vk::ImageCopy {
            src_subresource: buffer_region.image_subresource,
            src_offset: vk::Offset3D { x: 1, y: 1, z: 0 },
            dst_subresource: buffer_region.image_subresource,
            dst_offset: vk::Offset3D { x: 3, y: 2, z: 0 },
            extent: vk::Extent3D { width: 2, height: 2, depth: 1 },
        };
        vkCmdCopyImage(
            cb,
            src,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            dst,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            1,
            &image_region,
        );
        let blit_region = vk::ImageBlit {
            src_subresource: buffer_region.image_subresource,
            src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: 2, y: 2, z: 1 }],
            dst_subresource: buffer_region.image_subresource,
            dst_offsets: [vk::Offset3D { x: 4, y: 4, z: 0 }, vk::Offset3D { x: 8, y: 8, z: 1 }],
        };
        vkCmdBlitImage(
            cb,
            src,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            dst,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            1,
            &blit_region,
            vk::Filter::LINEAR,
        );
        let clear = vk::ClearColorValue { float32: [0.25, 0.5, 0.75, 1.0] };
        let clear_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        vkCmdClearColorImage(
            cb,
            dst,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &clear,
            1,
            &clear_range,
        );
        barrier(
            cb,
            dst,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            0,
            1,
            0,
            1,
        );
        vkCmdCopyImageToBuffer(
            cb,
            dst,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            readback,
            1,
            &buffer_region,
        );

        let key = cb as usize;
        let enc = reg::lock().cmdbufs[&key].enc.clone();
        assert!(enc.iter().any(|op| matches!(
            op,
            Enc::CopyBufferToTexture {
                src_offset: 16,
                bytes_per_row: 32,
                mip: 0,
                width: 4,
                height: 3,
                ..
            }
        )));
        assert!(enc.iter().any(|op| matches!(
            op,
            Enc::BlitTexture {
                src_origin: Origin3d { x: 0, y: 0, z: 0 },
                src_extent: Extent3d { width: 2, height: 2, depth: 1 },
                dst_origin: Origin3d { x: 4, y: 4, z: 0 },
                dst_extent: Extent3d { width: 4, height: 4, depth: 1 },
                filter: Filter::Linear,
                ..
            }
        )));
        assert!(enc.iter().any(|op| matches!(
            op,
            Enc::ClearRect { x: 0, y: 0, w: 8, h: 8, color, .. }
                if *color == [0.25, 0.5, 0.75, 1.0]
        )));
        assert!(enc.iter().any(|op| matches!(
            op,
            Enc::CopyTextureToTexture {
                src_origin: Origin3d { x: 1, y: 1, z: 0 },
                dst_origin: Origin3d { x: 3, y: 2, z: 0 },
                extent: Extent3d { width: 2, height: 2, depth: 1 },
                ..
            }
        )));
        assert!(enc.iter().any(|op| matches!(
            op,
            Enc::CopyTextureToBuffer {
                dst_offset: 16,
                bytes_per_row: 32,
                width: 4,
                height: 3,
                ..
            }
        )));

        // A multi-region call with an OOB tail records neither its valid head nor its invalid tail.
        let before = reg::lock().cmdbufs[&key].enc.len();
        let invalid = vk::ImageCopy {
            dst_offset: vk::Offset3D { x: 7, y: 7, z: 0 },
            extent: vk::Extent3D { width: 2, height: 2, depth: 1 },
            ..image_region
        };
        let regions = [image_region, invalid];
        vkCmdCopyImage(
            cb,
            src,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            dst,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            2,
            regions.as_ptr(),
        );
        assert_eq!(reg::lock().cmdbufs[&key].enc.len(), before);

        assert_eq!(vkEndCommandBuffer(cb), VK_SUCCESS);
        assert_eq!(submit(cb), VK_SUCCESS);
        crate::memory::vkDestroyBuffer(core::ptr::null_mut(), upload, core::ptr::null());
        crate::memory::vkDestroyBuffer(core::ptr::null_mut(), readback, core::ptr::null());
        crate::memory::vkDestroyImage(core::ptr::null_mut(), src, core::ptr::null());
        crate::memory::vkDestroyImage(core::ptr::null_mut(), dst, core::ptr::null());
    }
}

// ---- Vulkan 1.1: device-group command scope -----------------------------------------------------

/// `vkCmdSetDeviceMask` (Vulkan 1.1): select which devices of a group subsequent commands run on. We
/// expose a single-device group (mask 0x1), so this validates the recording state and is a no-op.
#[no_mangle]
pub extern "C" fn vkCmdSetDeviceMask(command_buffer: VkCommandBuffer, _device_mask: u32) {
    let _ = reg::lock().recording_mut(cb_key(command_buffer));
}

/// `vkCmdDispatchBase` (Vulkan 1.1): a compute dispatch with a base workgroup offset. With base (0,0,0)
/// it is exactly `vkCmdDispatch`; a non-zero base is not expressible in the current IR (no base-group
/// field), so it is recorded as an ordinary dispatch of `groupCount` groups (bounded — `partial`).
#[no_mangle]
pub extern "C" fn vkCmdDispatchBase(
    command_buffer: VkCommandBuffer,
    _base_group_x: u32,
    _base_group_y: u32,
    _base_group_z: u32,
    group_count_x: u32,
    group_count_y: u32,
    group_count_z: u32,
) {
    let mut s = reg::lock();
    if let Some(cb) = s.recording_mut(cb_key(command_buffer)) {
        let pipeline = cb.bound_pipeline;
        let groups: Vec<(u32, u32)> = cb.pending_bind_groups.clone();
        cb.enc.push(Enc::BeginComputePass);
        if let Some(p) = pipeline {
            cb.enc.push(Enc::SetPipeline(p));
        }
        for (index, group) in groups {
            cb.enc.push(Enc::SetBindGroup { index, group });
        }
        cb.enc.push(Enc::Dispatch { x: group_count_x, y: group_count_y, z: group_count_z });
        cb.enc.push(Enc::EndComputePass);
    }
}

// ---- Vulkan 1.2: render-pass-2 recording + draw-indirect-count ------------------------------------

/// `vkCmdBeginRenderPass2` (Vulkan 1.2): the `...2` begin — delegates to the 1.0 body using the
/// `VkSubpassBeginInfo::contents`. Ported from `MVKCmdBeginRenderPass` (shared 1.0/2 path).
#[no_mangle]
pub extern "C" fn vkCmdBeginRenderPass2(
    command_buffer: VkCommandBuffer,
    p_render_pass_begin: *const vk::RenderPassBeginInfo,
    p_subpass_begin_info: *const vk::SubpassBeginInfo,
) {
    let contents = unsafe { p_subpass_begin_info.as_ref() }.map(|s| s.contents.as_raw()).unwrap_or(0);
    vkCmdBeginRenderPass(command_buffer, p_render_pass_begin, contents);
}

/// `vkCmdEndRenderPass2` (Vulkan 1.2): delegates to the 1.0 end.
#[no_mangle]
pub extern "C" fn vkCmdEndRenderPass2(command_buffer: VkCommandBuffer, _p_subpass_end_info: *const c_void) {
    vkCmdEndRenderPass(command_buffer);
}

/// `vkCmdNextSubpass2` (Vulkan 1.2): delegates to the 1.0 subpass advance (single-subpass model).
#[no_mangle]
pub extern "C" fn vkCmdNextSubpass2(
    command_buffer: VkCommandBuffer,
    p_subpass_begin_info: *const vk::SubpassBeginInfo,
    _p_subpass_end_info: *const c_void,
) {
    let contents = unsafe { p_subpass_begin_info.as_ref() }.map(|s| s.contents.as_raw()).unwrap_or(0);
    vkCmdNextSubpass(command_buffer, contents);
}

/// `vkCmdDrawIndirectCount` (Vulkan 1.2 / VK_KHR_draw_indirect_count): draw with the draw count read
/// from `countBuffer`. Both parameter buffers are validated (INDIRECT usage, in-bounds read spans); the
/// IR has no indirect encoder op yet, so the draw itself is not lowered (bounded — `partial`).
#[no_mangle]
pub extern "C" fn vkCmdDrawIndirectCount(
    command_buffer: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    count_buffer: VkBuffer,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    let _ = command_buffer;
    let s = reg::lock();
    let _ok = indirect_ok(&s, buffer, offset, max_draw_count.max(1), stride, 16)
        && s.buffers.get(&count_buffer).is_some_and(|b| {
            b.usage & buffer_usage::INDIRECT != 0 && count_buffer_offset.checked_add(4).is_some_and(|e| e <= b.size)
        });
}

/// `vkCmdDrawIndexedIndirectCount` (Vulkan 1.2): as above for indexed draws (20-byte command struct).
#[no_mangle]
pub extern "C" fn vkCmdDrawIndexedIndirectCount(
    command_buffer: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    count_buffer: VkBuffer,
    count_buffer_offset: u64,
    max_draw_count: u32,
    stride: u32,
) {
    let _ = command_buffer;
    let s = reg::lock();
    let _ok = indirect_ok(&s, buffer, offset, max_draw_count.max(1), stride, 20)
        && s.buffers.get(&count_buffer).is_some_and(|b| {
            b.usage & buffer_usage::INDIRECT != 0 && count_buffer_offset.checked_add(4).is_some_and(|e| e <= b.size)
        });
}
