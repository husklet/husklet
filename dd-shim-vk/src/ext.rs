//! Modern device extensions wgpu-on-Vulkan / Zed require (real bodies):
//! `VK_KHR_timeline_semaphore`, `VK_KHR_dynamic_rendering`, `VK_KHR_buffer_device_address`.
//!
//! Ported from MoltenVK: `MVKTimelineSemaphore` (`MVKSync.mm`) for the timeline counter wait/signal/poll;
//! `MVKCmdBeginRendering`/`MVKCmdEndRendering` (`MVKCmdRenderPass.mm`) for the render-pass-object-free
//! dynamic-rendering path (lowered to the same IR `BeginRenderPass`/`EndRenderPass` as a classic pass);
//! `MVKBuffer::getDeviceAddress` for the buffer device address (a stable per-buffer handle-address).
//!
//! Each extension exposes both its promoted-core name (e.g. `vkWaitSemaphores`, core 1.2) and its
//! KHR/EXT-suffixed alias (e.g. `vkWaitSemaphoresKHR`); the alias delegates to the core body so a single
//! implementation serves both. The extensions are advertised in `crate::capability::ADVERTISED_DEVICE_EXTENSIONS`
//! only because they are really implemented here.

use crate::reg::{self, ImageEvent, ImageSubresourceRange};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_shim_common::ir::*;

// VkStructureType values consulted in pNext chains (stable ABI).
const SEMAPHORE_TYPE_CREATE_INFO: i32 = 1_000_207_002;
const TIMELINE_SEMAPHORE_SUBMIT_INFO: i32 = 1_000_207_003;

/// Walk a `pNext` chain (each node begins with `VkBaseInStructure { sType, pNext }`) for a target sType.
///
/// # Safety
/// `p` must be null or a valid pNext chain head whose nodes each begin with `VkBaseInStructure`.
pub(crate) unsafe fn find_pnext(mut p: *const c_void, target: i32) -> *const c_void {
    while !p.is_null() {
        let base = &*(p as *const vk::BaseInStructure);
        if base.s_type.as_raw() == target {
            return p;
        }
        p = base.p_next as *const c_void;
    }
    core::ptr::null()
}

/// Parse a `VkSemaphoreCreateInfo`'s pNext for `VkSemaphoreTypeCreateInfo` → `(is_timeline, initial_value)`.
pub fn parse_semaphore_type(p_create_info: *const c_void) -> (bool, u64) {
    let Some(ci) = (unsafe { (p_create_info as *const vk::SemaphoreCreateInfo).as_ref() }) else {
        return (false, 0);
    };
    let node = unsafe { find_pnext(ci.p_next as *const c_void, SEMAPHORE_TYPE_CREATE_INFO) };
    if node.is_null() {
        return (false, 0);
    }
    let ti = unsafe { &*(node as *const vk::SemaphoreTypeCreateInfo) };
    // VK_SEMAPHORE_TYPE_TIMELINE = 1.
    (ti.semaphore_type.as_raw() == 1, ti.initial_value)
}

/// The per-signal-semaphore timeline values from a submit's `VkTimelineSemaphoreSubmitInfo`, if present.
pub fn timeline_signal_values(p_next: *const c_void) -> Vec<u64> {
    let node = unsafe { find_pnext(p_next, TIMELINE_SEMAPHORE_SUBMIT_INFO) };
    if node.is_null() {
        return Vec::new();
    }
    let ts = unsafe { &*(node as *const vk::TimelineSemaphoreSubmitInfo) };
    if ts.p_signal_semaphore_values.is_null() {
        return Vec::new();
    }
    unsafe {
        core::slice::from_raw_parts(ts.p_signal_semaphore_values, ts.signal_semaphore_value_count as usize)
    }
    .to_vec()
}

/// The per-wait-semaphore timeline values from a submit's `VkTimelineSemaphoreSubmitInfo`, if present.
pub fn timeline_wait_values(p_next: *const c_void) -> Vec<u64> {
    let node = unsafe { find_pnext(p_next, TIMELINE_SEMAPHORE_SUBMIT_INFO) };
    if node.is_null() {
        return Vec::new();
    }
    let ts = unsafe { &*(node as *const vk::TimelineSemaphoreSubmitInfo) };
    if ts.p_wait_semaphore_values.is_null() {
        return Vec::new();
    }
    unsafe {
        core::slice::from_raw_parts(ts.p_wait_semaphore_values, ts.wait_semaphore_value_count as usize)
    }
    .to_vec()
}

// ---- VK_KHR_timeline_semaphore -------------------------------------------------------------------

/// `vkGetSemaphoreCounterValue` — read a timeline semaphore's current counter (MVKTimelineSemaphore).
#[no_mangle]
pub extern "C" fn vkGetSemaphoreCounterValue(
    _device: VkDevice,
    semaphore: VkSemaphore,
    p_value: *mut u64,
) -> VkResult {
    let s = reg::lock();
    match s.semaphores.get(&semaphore) {
        Some(sm) if sm.timeline => {
            if let Some(out) = unsafe { p_value.as_mut() } {
                *out = sm.counter;
            }
            VK_SUCCESS
        }
        _ => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// `vkSignalSemaphore` — host-side signal of a timeline semaphore to `value` (must be ≥ the current
/// counter; the counter only advances). Ported from `MVKTimelineSemaphore::signal`.
#[no_mangle]
pub extern "C" fn vkSignalSemaphore(_device: VkDevice, p_signal_info: *const vk::SemaphoreSignalInfo) -> VkResult {
    let Some(info) = (unsafe { p_signal_info.as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    match s.semaphores.get_mut(&info.semaphore.as_raw()) {
        Some(sm) if sm.timeline => {
            sm.counter = sm.counter.max(info.value);
            VK_SUCCESS
        }
        _ => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// `vkWaitSemaphores` — wait for timeline semaphores to reach their values. Our submit/host signals are
/// synchronous, so a satisfied wait returns immediately; an unsatisfiable wait honestly reports
/// `VK_TIMEOUT` (never blocks). `waitAll` (flags bit 0 == 0) vs any-of. Ported from `MVKDevice::waitSemaphores`.
#[no_mangle]
pub extern "C" fn vkWaitSemaphores(_device: VkDevice, p_wait_info: *const vk::SemaphoreWaitInfo, timeout: u64) -> VkResult {
    let Some(info) = (unsafe { p_wait_info.as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.semaphore_count == 0 || info.p_semaphores.is_null() || info.p_values.is_null() {
        return VK_SUCCESS;
    }
    let sems = unsafe { core::slice::from_raw_parts(info.p_semaphores, info.semaphore_count as usize) };
    let vals = unsafe { core::slice::from_raw_parts(info.p_values, info.semaphore_count as usize) };
    let s = reg::lock();
    let reached = |i: usize| s.semaphores.get(&sems[i].as_raw()).map(|sm| sm.counter >= vals[i]).unwrap_or(false);
    // VK_SEMAPHORE_WAIT_ANY_BIT = 0x1.
    let any = info.flags.as_raw() & 0x1 != 0;
    let satisfied = if any {
        (0..sems.len()).any(reached)
    } else {
        (0..sems.len()).all(reached)
    };
    if satisfied {
        VK_SUCCESS
    } else if timeout == 0 {
        VK_TIMEOUT
    } else {
        VK_TIMEOUT // synchronous model: unmet counters never advance here, so report the timeout truthfully
    }
}

#[no_mangle]
pub extern "C" fn vkGetSemaphoreCounterValueKHR(device: VkDevice, semaphore: VkSemaphore, p_value: *mut u64) -> VkResult {
    vkGetSemaphoreCounterValue(device, semaphore, p_value)
}
#[no_mangle]
pub extern "C" fn vkSignalSemaphoreKHR(device: VkDevice, p_signal_info: *const vk::SemaphoreSignalInfo) -> VkResult {
    vkSignalSemaphore(device, p_signal_info)
}
#[no_mangle]
pub extern "C" fn vkWaitSemaphoresKHR(device: VkDevice, p_wait_info: *const vk::SemaphoreWaitInfo, timeout: u64) -> VkResult {
    vkWaitSemaphores(device, p_wait_info, timeout)
}

// ---- VK_KHR_dynamic_rendering --------------------------------------------------------------------

/// `vkCmdBeginRendering` — begin a render-pass-object-free rendering scope. The first color attachment is
/// lowered to the same IR `BeginRenderPass` a classic pass produces (attachment texture + load/clear/store),
/// and its layout is tracked at submit exactly like a render pass. Ported from `MVKCmdBeginRendering`.
/// Bounded: one color attachment, no depth/stencil, no multiview.
#[no_mangle]
pub extern "C" fn vkCmdBeginRendering(command_buffer: VkCommandBuffer, p_rendering_info: *const vk::RenderingInfo) {
    let Some(info) = (unsafe { p_rendering_info.as_ref() }) else {
        return;
    };
    if info.color_attachment_count == 0 || info.p_color_attachments.is_null() {
        return;
    }
    let atts = unsafe { core::slice::from_raw_parts(info.p_color_attachments, info.color_attachment_count as usize) };
    let att = &atts[0];
    if att.image_view.is_null() {
        return;
    }
    let mut s = reg::lock();
    // Resolve imageView -> image -> IR texture id + subresource range.
    let Some((image, range, tex)) = s
        .image_views
        .get(&att.image_view.as_raw())
        .copied()
        .and_then(|iv| s.images.get(&iv.image).map(|im| (iv.image, iv.range, im.ir_id)))
    else {
        return;
    };
    // load/store + clear from the attachment; the layout during rendering is the attachment's imageLayout.
    let load = if att.load_op == vk::AttachmentLoadOp::CLEAR { LoadOp::Clear } else { LoadOp::Load };
    let store = att.store_op == vk::AttachmentStoreOp::STORE;
    let clear = unsafe { att.clear_value.color.float32 };
    let layout = att.image_layout.as_raw();
    let (w, h) = (
        info.render_area.offset.x.max(0) as u32 + info.render_area.extent.width,
        info.render_area.offset.y.max(0) as u32 + info.render_area.extent.height,
    );
    if let Some(cb) = s.recording_mut(command_buffer as usize) {
        cb.image_events.push(ImageEvent::RenderBegin {
            image,
            range,
            initial_layout: layout,
            subpass_layout: layout,
        });
        cb.active_render_image = Some((image, range, layout, layout));
        cb.enc.push(Enc::BeginRenderPass {
            color: vec![ColorAttachment { texture: tex, load, clear, store }],
            depth: None,
        });
        cb.enc.push(Enc::SetViewport { x: 0.0, y: 0.0, w: w as f32, h: h as f32, min_depth: 0.0, max_depth: 1.0 });
        cb.enc.push(Enc::SetScissor { x: 0, y: 0, w, h });
        cb.in_render_pass = true;
        cb.pipeline_set_in_pass = false;
    }
}

/// `vkCmdEndRendering` — end the dynamic-rendering scope (IR `EndRenderPass` + the final layout event).
#[no_mangle]
pub extern "C" fn vkCmdEndRendering(command_buffer: VkCommandBuffer) {
    if let Some(cb) = reg::lock().recording_mut(command_buffer as usize) {
        if let Some((image, range, subpass_layout, final_layout)) = cb.active_render_image.take() {
            cb.image_events.push(ImageEvent::RenderEnd { image, range, subpass_layout, final_layout });
        }
        cb.enc.push(Enc::EndRenderPass);
        cb.in_render_pass = false;
    }
}

#[no_mangle]
pub extern "C" fn vkCmdBeginRenderingKHR(command_buffer: VkCommandBuffer, p_rendering_info: *const vk::RenderingInfo) {
    vkCmdBeginRendering(command_buffer, p_rendering_info);
}
#[no_mangle]
pub extern "C" fn vkCmdEndRenderingKHR(command_buffer: VkCommandBuffer) {
    vkCmdEndRendering(command_buffer);
}

// ---- VK_KHR_buffer_device_address ----------------------------------------------------------------

/// A stable, unique synthetic device address for a buffer (derived from its IR id). Non-zero, page-ish
/// aligned. Ported from `MVKBuffer::getDeviceAddress` (bounded: a shader cannot yet DEREFERENCE it — the
/// address is a stable handle, not a mapped GPU VA).
fn synthetic_address(ir_id: u32) -> u64 {
    0x1_0000_0000u64 + (ir_id as u64) * 0x1_0000
}

/// `vkGetBufferDeviceAddress` — the buffer's device address. Ported from `MVKBuffer::getDeviceAddress`.
#[no_mangle]
pub extern "C" fn vkGetBufferDeviceAddress(_device: VkDevice, p_info: *const vk::BufferDeviceAddressInfo) -> u64 {
    let Some(info) = (unsafe { p_info.as_ref() }) else {
        return 0;
    };
    reg::lock().buffers.get(&info.buffer.as_raw()).map(|b| synthetic_address(b.ir_id)).unwrap_or(0)
}

/// `vkGetBufferOpaqueCaptureAddress` — no capture/replay support → 0 (spec-valid when unused).
#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureAddress(_device: VkDevice, _p_info: *const vk::BufferDeviceAddressInfo) -> u64 {
    0
}

/// `vkGetDeviceMemoryOpaqueCaptureAddress` — no capture/replay support → 0.
#[no_mangle]
pub extern "C" fn vkGetDeviceMemoryOpaqueCaptureAddress(
    _device: VkDevice,
    _p_info: *const vk::DeviceMemoryOpaqueCaptureAddressInfo,
) -> u64 {
    0
}

#[no_mangle]
pub extern "C" fn vkGetBufferDeviceAddressKHR(device: VkDevice, p_info: *const vk::BufferDeviceAddressInfo) -> u64 {
    vkGetBufferDeviceAddress(device, p_info)
}
#[no_mangle]
pub extern "C" fn vkGetBufferDeviceAddressEXT(device: VkDevice, p_info: *const vk::BufferDeviceAddressInfo) -> u64 {
    vkGetBufferDeviceAddress(device, p_info)
}
#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureAddressKHR(device: VkDevice, p_info: *const vk::BufferDeviceAddressInfo) -> u64 {
    vkGetBufferOpaqueCaptureAddress(device, p_info)
}
#[no_mangle]
pub extern "C" fn vkGetDeviceMemoryOpaqueCaptureAddressKHR(
    device: VkDevice,
    p_info: *const vk::DeviceMemoryOpaqueCaptureAddressInfo,
) -> u64 {
    vkGetDeviceMemoryOpaqueCaptureAddress(device, p_info)
}
