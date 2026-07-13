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

use crate::reg::{self, CommandBufferState, RenderPassRec};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_shim_common::ir::*;

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

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets(
    command_buffer: VkCommandBuffer,
    _pipeline_bind_point: i32,
    _layout: VkPipelineLayout,
    first_set: u32,
    descriptor_set_count: u32,
    p_descriptor_sets: *const VkDescriptorSet,
    _dynamic_offset_count: u32,
    _p_dynamic_offsets: *const u32,
) {
    if p_descriptor_sets.is_null() {
        return;
    }
    let sets = unsafe { core::slice::from_raw_parts(p_descriptor_sets, descriptor_set_count as usize) };
    let mut s = reg::lock();
    for (i, &dset) in sets.iter().enumerate() {
        let set_index = first_set + i as u32;
        // Build the bind group's entries from the descriptor set's (binding -> buffer) table,
        // resolving each buffer handle to its IR id.
        let Some(rec) = s.dsets.get(&dset) else { continue };
        let mut entries: Vec<BindEntry> = Vec::new();
        let mut pairs: Vec<(u32, (u64, u64, u64))> = rec.buffers.iter().map(|(b, v)| (*b, *v)).collect();
        pairs.sort_by_key(|(b, _)| *b);
        for (binding, (buf_handle, offset, size)) in pairs {
            if let Some(b) = s.buffers.get(&buf_handle) {
                entries.push(BindEntry {
                    binding,
                    resource: BindResource::Buffer {
                        id: b.ir_id,
                        offset,
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
    // Resolve the color attachment IR texture id + framebuffer dims.
    let rp: Option<RenderPassRec> = s.render_passes.get(&bi.render_pass.as_raw()).map(|r| RenderPassRec {
        color_format: r.color_format,
        color_load_clear: r.color_load_clear,
        clear: r.clear,
        color_store: r.color_store,
    });
    let fb = s.framebuffers.get(&bi.framebuffer.as_raw());
    let (fb_w, fb_h, color_view) = match fb {
        Some(f) => (f.width, f.height, f.color_view),
        None => (0, 0, None),
    };
    let attach_ir = color_view
        .and_then(|v| s.image_views.get(&v).map(|iv| iv.image))
        .and_then(|img| s.images.get(&img).map(|im| im.ir_id));

    // Clear color from pClearValues[0].
    let clear = if bi.clear_value_count > 0 && !bi.p_clear_values.is_null() {
        let cv = unsafe { &*bi.p_clear_values };
        unsafe { cv.color.float32 }
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let load = match rp.as_ref().map(|r| r.color_load_clear).unwrap_or(true) {
        true => LoadOp::Clear,
        false => LoadOp::Load,
    };
    let store = rp.as_ref().map(|r| r.color_store).unwrap_or(true);

    if let (Some(tex), Some(cb)) = (attach_ir, s.recording_mut(cb_key(command_buffer))) {
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

// ---- submit + sync -------------------------------------------------------------------------------

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
    let mut signal_sems: Vec<u64> = Vec::new();
    for sub in submits {
        // Wait semaphores must all exist and be signaled (else the batch cannot proceed).
        if !sub.p_wait_semaphores.is_null() {
            let waits = unsafe {
                core::slice::from_raw_parts(sub.p_wait_semaphores, sub.wait_semaphore_count as usize)
            };
            for &w in waits {
                match s.semaphores.get(&w.as_raw()) {
                    Some(sm) if sm.signaled => wait_sems.push(w.as_raw()),
                    _ => return VK_ERROR_INITIALIZATION_FAILED,
                }
            }
        }
        if !sub.p_signal_semaphores.is_null() {
            let sigs = unsafe {
                core::slice::from_raw_parts(sub.p_signal_semaphores, sub.signal_semaphore_count as usize)
            };
            for &sg in sigs {
                signal_sems.push(sg.as_raw());
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

    // ---- phase 2: consume waits, ship the IR, move buffers Pending ----
    for w in &wait_sems {
        if let Some(sm) = s.semaphores.get_mut(w) {
            sm.signaled = false; // binary semaphore reset on wait
        }
    }
    // Flush persistently-mapped HOST_COHERENT buffers (vkcube writes its rotating MVP UBO every frame
    // without unmapping) so the host sees this frame's uniform data before the draw replays.
    s.flush_mapped();
    // The host Metal executor renders synchronously and does not model fence objects, so we never
    // signal a fence *in the shipped IR* — the fence/semaphore state machine is guest-side (below).
    let mut encoders: Vec<Vec<Enc>> = Vec::new();
    for &key in &cb_keys {
        if let Some(rec) = s.cmdbufs.get(&key) {
            encoders.push(rec.enc.clone());
        }
    }
    for encoder in encoders {
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
    for sg in &signal_sems {
        if let Some(sm) = s.semaphores.get_mut(sg) {
            sm.signaled = true;
        }
    }
    if fence != 0 {
        if let Some(f) = s.fences.get_mut(&fence) {
            f.signaled = true;
        }
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
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_semaphore: *mut VkSemaphore,
) -> VkResult {
    let Some(out) = (unsafe { p_semaphore.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.semaphores.insert(handle, reg::SemaphoreRec::default());
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySemaphore(_device: VkDevice, semaphore: VkSemaphore, _p_allocator: *const c_void) {
    reg::lock().semaphores.remove(&semaphore);
}
