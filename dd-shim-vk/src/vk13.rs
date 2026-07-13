//! Vulkan 1.3 promoted-core commands (real bodies), ported from MoltenVK.
//!
//! Grouped by the extensions 1.3 promoted:
//!  * **extended dynamic state 1+2** (`MVKCommandEncoderState`): the `vkCmdSet*` state setters record
//!    verbatim into the command buffer's dynamic state (observable; lowering into the IR draw state is a
//!    later increment — classified `partial`, like the 1.0 dynamic setters).
//!  * **copy commands 2** (`MVKCmdCopy*`): the `...2` copy/blit/resolve forms carry the same regions as
//!    the 1.0 commands inside `sType`/`pNext`-tagged structs, so they convert the region arrays and
//!    delegate to the validated 1.0 bodies.
//!  * **synchronization2** (`MVKCmdPipelineBarrier`/`MVKQueue`): `vkCmdSetEvent2`/`ResetEvent2`/
//!    `WaitEvents2`/`WriteTimestamp2` and `vkQueueSubmit2` map onto the existing event/barrier/submit
//!    state machines.
//!  * **maintenance4** (`MVKDevice`): device-level memory requirements from a create-info (no object).
//!  * **private data** (`MVKPrivateDataSlot`): a per-`(slot, objectType, objectHandle)` u64 payload.
//!  * **tool properties**: no active tool layers → an empty list.

use crate::reg::{self, DeferredOp};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;

// ---- extended dynamic state (EDS1 + EDS2) --------------------------------------------------------

macro_rules! set_dyn {
    ($name:ident ( $cb:ident, $val:ident : $ty:ty ) => $field:expr) => {
        #[no_mangle]
        pub extern "C" fn $name(command_buffer: VkCommandBuffer, $val: $ty) {
            if let Some(cb) = reg::lock().recording_mut(command_buffer as usize) {
                let $cb = cb;
                $field;
            }
        }
    };
}

set_dyn!(vkCmdSetCullMode(cb, cull_mode: vk::CullModeFlags) => cb.dynamic.cull_mode = cull_mode.as_raw());
set_dyn!(vkCmdSetFrontFace(cb, front_face: vk::FrontFace) => cb.dynamic.front_face = front_face.as_raw());
set_dyn!(vkCmdSetPrimitiveTopology(cb, topology: vk::PrimitiveTopology) => cb.dynamic.primitive_topology = topology.as_raw());
set_dyn!(vkCmdSetPrimitiveRestartEnable(cb, enable: vk::Bool32) => cb.dynamic.primitive_restart_enable = enable != 0);
set_dyn!(vkCmdSetRasterizerDiscardEnable(cb, enable: vk::Bool32) => cb.dynamic.rasterizer_discard_enable = enable != 0);
set_dyn!(vkCmdSetDepthTestEnable(cb, enable: vk::Bool32) => cb.dynamic.depth_test_enable = enable != 0);
set_dyn!(vkCmdSetDepthWriteEnable(cb, enable: vk::Bool32) => cb.dynamic.depth_write_enable = enable != 0);
set_dyn!(vkCmdSetDepthCompareOp(cb, op: vk::CompareOp) => cb.dynamic.depth_compare_op = op.as_raw());
set_dyn!(vkCmdSetDepthBoundsTestEnable(cb, enable: vk::Bool32) => cb.dynamic.depth_bounds_test_enable = enable != 0);
set_dyn!(vkCmdSetDepthBiasEnable(cb, enable: vk::Bool32) => cb.dynamic.depth_bias_enable = enable != 0);
set_dyn!(vkCmdSetStencilTestEnable(cb, enable: vk::Bool32) => cb.dynamic.stencil_test_enable = enable != 0);

#[no_mangle]
pub extern "C" fn vkCmdSetStencilOp(
    command_buffer: VkCommandBuffer,
    face_mask: vk::StencilFaceFlags,
    fail_op: vk::StencilOp,
    pass_op: vk::StencilOp,
    depth_fail_op: vk::StencilOp,
    compare_op: vk::CompareOp,
) {
    if let Some(cb) = reg::lock().recording_mut(command_buffer as usize) {
        cb.dynamic.stencil_op = (
            face_mask.as_raw(),
            fail_op.as_raw(),
            pass_op.as_raw(),
            depth_fail_op.as_raw(),
            compare_op.as_raw(),
        );
    }
}

/// `vkCmdSetViewportWithCount` — record the count and lower the first viewport (the IR carries one).
#[no_mangle]
pub extern "C" fn vkCmdSetViewportWithCount(
    command_buffer: VkCommandBuffer,
    viewport_count: u32,
    p_viewports: *const vk::Viewport,
) {
    if let Some(cb) = reg::lock().recording_mut(command_buffer as usize) {
        cb.dynamic.viewport_count = viewport_count;
    }
    crate::command::vkCmdSetViewport(command_buffer, 0, viewport_count, p_viewports);
}

/// `vkCmdSetScissorWithCount` — record the count and lower the first scissor.
#[no_mangle]
pub extern "C" fn vkCmdSetScissorWithCount(
    command_buffer: VkCommandBuffer,
    scissor_count: u32,
    p_scissors: *const vk::Rect2D,
) {
    if let Some(cb) = reg::lock().recording_mut(command_buffer as usize) {
        cb.dynamic.scissor_count = scissor_count;
    }
    crate::command::vkCmdSetScissor(command_buffer, 0, scissor_count, p_scissors);
}

/// `vkCmdBindVertexBuffers2` — the EDS form with per-binding sizes/strides; the IR uses buffer+offset, so
/// it delegates to the 1.0 bind (sizes/strides are validated-by-ignore in this bring-up).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdBindVertexBuffers2(
    command_buffer: VkCommandBuffer,
    first_binding: u32,
    binding_count: u32,
    p_buffers: *const VkBuffer,
    p_offsets: *const u64,
    _p_sizes: *const u64,
    _p_strides: *const u64,
) {
    crate::command::vkCmdBindVertexBuffers(command_buffer, first_binding, binding_count, p_buffers, p_offsets);
}

// ---- copy commands 2 -----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCmdCopyBuffer2(command_buffer: VkCommandBuffer, p_info: *const vk::CopyBufferInfo2) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let regions = slice(info.p_regions, info.region_count);
    let r1: Vec<vk::BufferCopy> = regions
        .iter()
        .map(|r| vk::BufferCopy { src_offset: r.src_offset, dst_offset: r.dst_offset, size: r.size })
        .collect();
    crate::command::vkCmdCopyBuffer(command_buffer, info.src_buffer.as_raw(), info.dst_buffer.as_raw(), r1.len() as u32, r1.as_ptr());
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImage2(command_buffer: VkCommandBuffer, p_info: *const vk::CopyImageInfo2) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let regions = slice(info.p_regions, info.region_count);
    let r1: Vec<vk::ImageCopy> = regions
        .iter()
        .map(|r| vk::ImageCopy {
            src_subresource: r.src_subresource,
            src_offset: r.src_offset,
            dst_subresource: r.dst_subresource,
            dst_offset: r.dst_offset,
            extent: r.extent,
        })
        .collect();
    crate::command::vkCmdCopyImage(
        command_buffer,
        info.src_image.as_raw(),
        info.src_image_layout,
        info.dst_image.as_raw(),
        info.dst_image_layout,
        r1.len() as u32,
        r1.as_ptr(),
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyBufferToImage2(command_buffer: VkCommandBuffer, p_info: *const vk::CopyBufferToImageInfo2) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let r1 = buffer_image_regions(info.p_regions, info.region_count);
    crate::command::vkCmdCopyBufferToImage(
        command_buffer,
        info.src_buffer.as_raw(),
        info.dst_image.as_raw(),
        info.dst_image_layout,
        r1.len() as u32,
        r1.as_ptr(),
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyImageToBuffer2(command_buffer: VkCommandBuffer, p_info: *const vk::CopyImageToBufferInfo2) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let r1 = buffer_image_regions(info.p_regions, info.region_count);
    crate::command::vkCmdCopyImageToBuffer(
        command_buffer,
        info.src_image.as_raw(),
        info.src_image_layout,
        info.dst_buffer.as_raw(),
        r1.len() as u32,
        r1.as_ptr(),
    );
}

#[no_mangle]
pub extern "C" fn vkCmdBlitImage2(command_buffer: VkCommandBuffer, p_info: *const vk::BlitImageInfo2) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let regions = slice(info.p_regions, info.region_count);
    let r1: Vec<vk::ImageBlit> = regions
        .iter()
        .map(|r| vk::ImageBlit {
            src_subresource: r.src_subresource,
            src_offsets: r.src_offsets,
            dst_subresource: r.dst_subresource,
            dst_offsets: r.dst_offsets,
        })
        .collect();
    crate::command::vkCmdBlitImage(
        command_buffer,
        info.src_image.as_raw(),
        info.src_image_layout,
        info.dst_image.as_raw(),
        info.dst_image_layout,
        r1.len() as u32,
        r1.as_ptr(),
        info.filter,
    );
}

#[no_mangle]
pub extern "C" fn vkCmdResolveImage2(command_buffer: VkCommandBuffer, p_info: *const vk::ResolveImageInfo2) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let regions = slice(info.p_regions, info.region_count);
    let r1: Vec<vk::ImageResolve> = regions
        .iter()
        .map(|r| vk::ImageResolve {
            src_subresource: r.src_subresource,
            src_offset: r.src_offset,
            dst_subresource: r.dst_subresource,
            dst_offset: r.dst_offset,
            extent: r.extent,
        })
        .collect();
    crate::command::vkCmdResolveImage(
        command_buffer,
        info.src_image.as_raw(),
        info.src_image_layout,
        info.dst_image.as_raw(),
        info.dst_image_layout,
        r1.len() as u32,
        r1.as_ptr(),
    );
}

fn slice<'a, T>(ptr: *const T, count: u32) -> &'a [T] {
    if ptr.is_null() || count == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, count as usize) }
    }
}

fn buffer_image_regions(ptr: *const vk::BufferImageCopy2, count: u32) -> Vec<vk::BufferImageCopy> {
    slice(ptr, count)
        .iter()
        .map(|r| vk::BufferImageCopy {
            buffer_offset: r.buffer_offset,
            buffer_row_length: r.buffer_row_length,
            buffer_image_height: r.buffer_image_height,
            image_subresource: r.image_subresource,
            image_offset: r.image_offset,
            image_extent: r.image_extent,
        })
        .collect()
}

// ---- synchronization2 commands -------------------------------------------------------------------

fn record_event(command_buffer: VkCommandBuffer, event: VkEvent, set: bool) {
    let mut s = reg::lock();
    if !s.events.contains_key(&event) {
        return;
    }
    if let Some(cb) = s.recording_mut(command_buffer as usize) {
        cb.deferred.push(DeferredOp::Event { event, set });
    }
}

/// `vkCmdSetEvent2` — set the event on completion + apply the dependency's image barriers.
#[no_mangle]
pub extern "C" fn vkCmdSetEvent2(command_buffer: VkCommandBuffer, event: VkEvent, p_dependency_info: *const vk::DependencyInfo) {
    record_event(command_buffer, event, true);
    crate::command::vkCmdPipelineBarrier2(command_buffer, p_dependency_info);
}

/// `vkCmdResetEvent2` — reset the event on completion.
#[no_mangle]
pub extern "C" fn vkCmdResetEvent2(command_buffer: VkCommandBuffer, event: VkEvent, _stage_mask: vk::PipelineStageFlags2) {
    record_event(command_buffer, event, false);
}

/// `vkCmdWaitEvents2` — wait on events, then apply each dependency's image barriers. The waited events
/// must exist; in this single-queue synchronous model the wait is satisfied by submit completion.
#[no_mangle]
pub extern "C" fn vkCmdWaitEvents2(
    command_buffer: VkCommandBuffer,
    event_count: u32,
    p_events: *const VkEvent,
    p_dependency_infos: *const vk::DependencyInfo,
) {
    {
        let s = reg::lock();
        let events = slice(p_events, event_count);
        if !events.iter().all(|e| s.events.contains_key(e)) {
            return;
        }
    }
    for dep in slice(p_dependency_infos, event_count) {
        crate::command::vkCmdPipelineBarrier2(command_buffer, dep);
    }
}

/// `vkCmdWriteTimestamp2` — delegate to the 1.0 timestamp recorder (the sync2 stage mask is ignored).
#[no_mangle]
pub extern "C" fn vkCmdWriteTimestamp2(
    command_buffer: VkCommandBuffer,
    _stage: vk::PipelineStageFlags2,
    query_pool: VkQueryPool,
    query: u32,
) {
    crate::query::vkCmdWriteTimestamp(command_buffer, vk::PipelineStageFlags::TOP_OF_PIPE, query_pool, query);
}

/// `vkQueueSubmit2` — the sync2 submit form. Translates each `VkSubmitInfo2` (semaphore/command-buffer
/// info structs) into the 1.0 `VkSubmitInfo` + `VkTimelineSemaphoreSubmitInfo` and drives the existing
/// `vkQueueSubmit` state machine (which handles binary + timeline semaphores + the fence).
#[no_mangle]
pub extern "C" fn vkQueueSubmit2(
    queue: VkQueue,
    submit_count: u32,
    p_submits: *const vk::SubmitInfo2,
    fence: VkFence,
) -> VkResult {
    let submits = slice(p_submits, submit_count);
    if submits.is_empty() {
        return crate::command::vkQueueSubmit(queue, 0, core::ptr::null(), fence);
    }
    for (i, sub) in submits.iter().enumerate() {
        let waits = slice(sub.p_wait_semaphore_infos, sub.wait_semaphore_info_count);
        let cbs_info = slice(sub.p_command_buffer_infos, sub.command_buffer_info_count);
        let sigs = slice(sub.p_signal_semaphore_infos, sub.signal_semaphore_info_count);
        let wait_s: Vec<vk::Semaphore> = waits.iter().map(|w| w.semaphore).collect();
        let wait_v: Vec<u64> = waits.iter().map(|w| w.value).collect();
        let wait_stage: Vec<vk::PipelineStageFlags> = vec![vk::PipelineStageFlags::empty(); wait_s.len()];
        let cbs: Vec<vk::CommandBuffer> = cbs_info.iter().map(|c| c.command_buffer).collect();
        let sig_s: Vec<vk::Semaphore> = sigs.iter().map(|s| s.semaphore).collect();
        let sig_v: Vec<u64> = sigs.iter().map(|s| s.value).collect();
        let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_v)
            .signal_semaphore_values(&sig_v);
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&wait_s)
            .wait_dst_stage_mask(&wait_stage)
            .command_buffers(&cbs)
            .signal_semaphores(&sig_s)
            .push_next(&mut timeline);
        // Signal the fence only on the final batch (a single fence signals once, on completion).
        let fence_arg = if i + 1 == submits.len() { fence } else { 0 };
        let r = crate::command::vkQueueSubmit(queue, 1, &submit, fence_arg);
        if r != VK_SUCCESS {
            return r;
        }
    }
    VK_SUCCESS
}

// ---- maintenance4: device-level memory requirements ----------------------------------------------

/// `vkGetDeviceBufferMemoryRequirements` (maintenance4) — the requirements for a buffer that WOULD be
/// created with the given `VkBufferCreateInfo`, without creating one. Same 256-aligned unified-type
/// layout as `vkGetBufferMemoryRequirements`.
#[no_mangle]
pub extern "C" fn vkGetDeviceBufferMemoryRequirements(
    _device: VkDevice,
    p_info: *const vk::DeviceBufferMemoryRequirements,
    p_mem_reqs: *mut vk::MemoryRequirements2,
) {
    let (Some(info), Some(out)) = (unsafe { p_info.as_ref() }, unsafe { p_mem_reqs.as_mut() }) else { return };
    let size = unsafe { info.p_create_info.as_ref() }.map(|ci| ci.size).unwrap_or(0);
    out.memory_requirements = vk::MemoryRequirements { size, alignment: 256, memory_type_bits: 0b1 };
}

/// `vkGetDeviceImageMemoryRequirements` (maintenance4) — as above from a `VkImageCreateInfo` (tightly
/// packed RGBA8 size).
#[no_mangle]
pub extern "C" fn vkGetDeviceImageMemoryRequirements(
    _device: VkDevice,
    p_info: *const vk::DeviceImageMemoryRequirements,
    p_mem_reqs: *mut vk::MemoryRequirements2,
) {
    let (Some(info), Some(out)) = (unsafe { p_info.as_ref() }, unsafe { p_mem_reqs.as_mut() }) else { return };
    let size = unsafe { info.p_create_info.as_ref() }
        .map(|ci| (ci.extent.width as u64) * (ci.extent.height as u64) * 4)
        .unwrap_or(0);
    out.memory_requirements = vk::MemoryRequirements { size, alignment: 256, memory_type_bits: 0b1 };
}

/// `vkGetDeviceImageSparseMemoryRequirements` (maintenance4) — no sparse residency → zero requirements.
#[no_mangle]
pub extern "C" fn vkGetDeviceImageSparseMemoryRequirements(
    _device: VkDevice,
    _p_info: *const c_void,
    p_count: *mut u32,
    _p_reqs: *mut c_void,
) {
    if let Some(c) = unsafe { p_count.as_mut() } {
        *c = 0;
    }
}

// ---- private data --------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreatePrivateDataSlot(
    _device: VkDevice,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_slot: *mut u64,
) -> VkResult {
    let Some(out) = (unsafe { p_slot.as_mut() }) else { return VK_ERROR_INITIALIZATION_FAILED };
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.private_data_slots.insert(handle);
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyPrivateDataSlot(_device: VkDevice, slot: u64, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    s.private_data_slots.remove(&slot);
    s.private_data.retain(|&(sl, _, _), _| sl != slot);
}

#[no_mangle]
pub extern "C" fn vkSetPrivateData(
    _device: VkDevice,
    object_type: i32,
    object_handle: u64,
    private_data_slot: u64,
    data: u64,
) -> VkResult {
    let mut s = reg::lock();
    if !s.private_data_slots.contains(&private_data_slot) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    s.private_data.insert((private_data_slot, object_type, object_handle), data);
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPrivateData(
    _device: VkDevice,
    object_type: i32,
    object_handle: u64,
    private_data_slot: u64,
    p_data: *mut u64,
) {
    let val = reg::lock()
        .private_data
        .get(&(private_data_slot, object_type, object_handle))
        .copied()
        .unwrap_or(0);
    if let Some(out) = unsafe { p_data.as_mut() } {
        *out = val;
    }
}

// ---- tool properties -----------------------------------------------------------------------------

/// `vkGetPhysicalDeviceToolProperties` (1.3) — no active tool layers (validation/debug), so report an
/// empty list (`*pToolCount = 0`).
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceToolProperties(
    _physical_device: VkPhysicalDevice,
    p_tool_count: *mut u32,
    _p_tool_properties: *mut c_void,
) -> VkResult {
    if let Some(c) = unsafe { p_tool_count.as_mut() } {
        *c = 0;
    }
    VK_SUCCESS
}
