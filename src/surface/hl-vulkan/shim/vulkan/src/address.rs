//! Buffer / device-memory device addresses (`VK_KHR_buffer_device_address` / core 1.2 +
//! `VK_EXT_buffer_device_address` + the opaque-capture-address queries).
//!
//! A device address is a stable, non-zero, per-buffer (per-memory) modeled `u64`: the shim derives it
//! from the object's stable id so the SAME buffer always maps to the SAME address across calls, and two
//! distinct buffers never collide. The address is a modeled token — the software backend does not expose
//! GPU virtual memory, so nothing dereferences it — but it is real, stable, and honest (never a bogus 0
//! for a live buffer). An unknown buffer/memory returns 0 (`VK_NULL` address), the truthful answer.

use core::ffi::c_void;

use hl_vulkan::Device;

use crate::state::with;

/// `VkBufferDeviceAddressInfo` / `VkDeviceMemoryOpaqueCaptureAddressInfo` share a head: `sType` (+4 pad
/// on LP64), `pNext`, then the single non-dispatchable handle (`buffer` or `memory`) at byte offset 16.
#[repr(C)]
struct HandleInfoHead {
    s_type: i32,
    _pad: u32,
    p_next: *const c_void,
    handle: u64,
}

/// Read the trailing `u64` handle from a `Vk*Info` whose only field after `pNext` is that handle.
unsafe fn info_handle(p_info: *const c_void) -> Option<u64> {
    (p_info as *const HandleInfoHead).as_ref().map(|h| h.handle)
}

/// The stable modeled device address of buffer `handle`, or 0 if unknown. Derived from the buffer's
/// stable IR id so it is identical across calls and unique per live buffer; high base keeps it clear of
/// small host pointers.
fn buffer_address(dev: &Device, handle: u64) -> u64 {
    match dev.buffers.get(&handle) {
        Some(b) => 0x0000_4000_0000_0000u64 + (b.ir_id as u64) * 0x1_0000,
        None => 0,
    }
}

/// The stable modeled opaque capture address of memory `handle`, or 0 if unknown.
fn memory_address(dev: &Device, handle: u64) -> u64 {
    if dev.memories.contains_key(&handle) {
        0x0000_5000_0000_0000u64 + handle
    } else {
        0
    }
}

// ---- vkGetBufferDeviceAddress (+ KHR/EXT aliases) ----------------------------------------------

#[no_mangle]
pub extern "C" fn vkGetBufferDeviceAddress(_device: *mut c_void, p_info: *const c_void) -> u64 {
    let Some(handle) = (unsafe { info_handle(p_info) }) else { return 0 };
    with(|s| s.device.as_ref().map(|d| buffer_address(d, handle)).unwrap_or(0))
}
#[no_mangle]
pub extern "C" fn vkGetBufferDeviceAddressKHR(device: *mut c_void, p_info: *const c_void) -> u64 {
    vkGetBufferDeviceAddress(device, p_info)
}
#[no_mangle]
pub extern "C" fn vkGetBufferDeviceAddressEXT(device: *mut c_void, p_info: *const c_void) -> u64 {
    vkGetBufferDeviceAddress(device, p_info)
}

// ---- vkGetBufferOpaqueCaptureAddress (+ KHR) ---------------------------------------------------
// The opaque capture address is a stable per-buffer token used for capture/replay; we return the same
// stable modeled address as the device address (a valid, consistent choice for a single-device model).

#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureAddress(_device: *mut c_void, p_info: *const c_void) -> u64 {
    let Some(handle) = (unsafe { info_handle(p_info) }) else { return 0 };
    with(|s| s.device.as_ref().map(|d| buffer_address(d, handle)).unwrap_or(0))
}
#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureAddressKHR(device: *mut c_void, p_info: *const c_void) -> u64 {
    vkGetBufferOpaqueCaptureAddress(device, p_info)
}

// ---- vkGetDeviceMemoryOpaqueCaptureAddress (+ KHR) ---------------------------------------------

#[no_mangle]
pub extern "C" fn vkGetDeviceMemoryOpaqueCaptureAddress(_device: *mut c_void, p_info: *const c_void) -> u64 {
    let Some(handle) = (unsafe { info_handle(p_info) }) else { return 0 };
    with(|s| s.device.as_ref().map(|d| memory_address(d, handle)).unwrap_or(0))
}
#[no_mangle]
pub extern "C" fn vkGetDeviceMemoryOpaqueCaptureAddressKHR(device: *mut c_void, p_info: *const c_void) -> u64 {
    vkGetDeviceMemoryOpaqueCaptureAddress(device, p_info)
}
