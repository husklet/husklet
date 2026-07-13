//! Device / queue / command-pool entry points (real bodies).
//!
//! Ported from MoltenVK's `MVKDevice` / `MVKQueue` / `MVKCommandPool` object model. A `VkDevice`
//! owns one `VkQueue` (our single graphics+compute+transfer family); command pools are
//! non-dispatchable handles, command buffers are dispatchable (loader-magic'd). Real submission /
//! recording lowers to dd-gpu IR in a later increment — this increment establishes the object graph
//! and the ABI-correct lifecycle (see `crate::ir_seam` for the IR mapping sketch + round-trip test).

use crate::handle::{self, Dispatchable};
use crate::reg;
use crate::state::{CommandBuffer, CommandPool, Device, Instance, PhysicalDevice, Queue};
use crate::types::*;
use ash::vk;
use ash::vk::Handle; // `.as_raw()` on non-dispatchable handle newtypes (VkCommandPool -> u64)
use core::ffi::c_void;

/// Create the logical device + its single queue (both loader-magic'd dispatchable objects).
#[no_mangle]
pub extern "C" fn vkCreateDevice(
    physical_device: VkPhysicalDevice,
    _p_create_info: *const vk::DeviceCreateInfo,
    _p_allocator: *const c_void,
    p_device: *mut VkDevice,
) -> VkResult {
    let Some(out) = (unsafe { p_device.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if physical_device.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let queue = Dispatchable::new(Queue {
        family_index: crate::state::QUEUE_FAMILY_INDEX,
        queue_index: 0,
    });
    let device = Dispatchable::new(Device {
        physical_device,
        queue,
    });
    *out = device;
    VK_SUCCESS
}

/// Tear down the device and its queue.
#[no_mangle]
pub extern "C" fn vkDestroyDevice(device: VkDevice, _p_allocator: *const c_void) {
    if device.is_null() {
        return;
    }
    unsafe {
        if let Some(dev) = Dispatchable::<Device>::inner(device) {
            Dispatchable::<Queue>::free(dev.queue);
        }
        Dispatchable::<Device>::free(device);
    }
}

/// Return the device's queue. The single family (index 0) has exactly one queue (index 0).
#[no_mangle]
pub extern "C" fn vkGetDeviceQueue(
    device: VkDevice,
    _queue_family_index: u32,
    _queue_index: u32,
    p_queue: *mut VkQueue,
) {
    if let (Some(dev), Some(out)) = unsafe { (Dispatchable::<Device>::inner(device), p_queue.as_mut()) } {
        *out = dev.queue;
    }
}

/// Create a command pool (non-dispatchable 64-bit handle over a boxed [`CommandPool`]).
#[no_mangle]
pub extern "C" fn vkCreateCommandPool(
    _device: VkDevice,
    p_create_info: *const vk::CommandPoolCreateInfo,
    _p_allocator: *const c_void,
    p_command_pool: *mut VkCommandPool,
) -> VkResult {
    let Some(out) = (unsafe { p_command_pool.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let qfi = unsafe { p_create_info.as_ref() }
        .map(|ci| ci.queue_family_index)
        .unwrap_or(crate::state::QUEUE_FAMILY_INDEX);
    *out = handle::nondispatch_new(CommandPool {
        queue_family_index: qfi,
    });
    VK_SUCCESS
}

/// Destroy a command pool.
#[no_mangle]
pub extern "C" fn vkDestroyCommandPool(
    _device: VkDevice,
    command_pool: VkCommandPool,
    _p_allocator: *const c_void,
) {
    unsafe { handle::nondispatch_free::<CommandPool>(command_pool) };
}

/// Allocate `commandBufferCount` dispatchable command buffers from the pool.
#[no_mangle]
pub extern "C" fn vkAllocateCommandBuffers(
    _device: VkDevice,
    p_allocate_info: *const vk::CommandBufferAllocateInfo,
    p_command_buffers: *mut VkCommandBuffer,
) -> VkResult {
    let Some(ai) = (unsafe { p_allocate_info.as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_command_buffers.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let pool = ai.command_pool.as_raw();
    let count = ai.command_buffer_count as usize;
    // Register each buffer pool-owned in the `Initial` state (MoltenVK `MVKCommandPool::allocate`), so
    // the command-buffer lifecycle state machine (crate::command) has a record from the moment of
    // allocation — begin/end/submit validate against it, and free removes it (below).
    let mut s = reg::lock();
    for i in 0..count {
        let cb = Dispatchable::new(CommandBuffer { pool });
        s.cmdbufs.insert(cb as usize, reg::CmdBufRec::initial());
        unsafe { *p_command_buffers.add(i) = cb };
    }
    VK_SUCCESS
}

/// Free command buffers allocated from a pool — remove the pool-owned recording record (so the address
/// is not left dangling/reusable) and release the dispatchable object.
#[no_mangle]
pub extern "C" fn vkFreeCommandBuffers(
    _device: VkDevice,
    _command_pool: VkCommandPool,
    command_buffer_count: u32,
    p_command_buffers: *const VkCommandBuffer,
) {
    if p_command_buffers.is_null() {
        return;
    }
    let mut s = reg::lock();
    for i in 0..command_buffer_count as usize {
        let cb = unsafe { *p_command_buffers.add(i) };
        s.cmdbufs.remove(&(cb as usize));
        unsafe { Dispatchable::<CommandBuffer>::free(cb) };
    }
}

// Silence unused-type lints for aliases referenced only in signatures elsewhere.
const _: Option<&Instance> = None;
const _: Option<&PhysicalDevice> = None;
