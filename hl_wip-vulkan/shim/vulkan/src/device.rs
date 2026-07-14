//! Logical-device + queue bring-up: `vkCreateDevice` / `vkDestroyDevice` / `vkGetDeviceQueue`.
//!
//! `vkCreateDevice` builds the `hl_vulkan::Device` (the object model + lowering target) over the
//! instance's physical device and stores it in the process-global [`crate::state`]. The device + queue
//! handles are loader-magic'd dispatchable tokens.

use core::ffi::c_void;

use hl_vulkan::service::create;

use crate::state::with;
use crate::types::*;

#[no_mangle]
pub extern "C" fn vkCreateDevice(
    _physical_device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_device: *mut *mut c_void,
) -> VkResult {
    if p_device.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let token = with(|s| {
        // Build the logical device over the instance's physical device (materialize a default instance
        // if a device is somehow requested before `vkCreateInstance`).
        let inst = s
            .instance
            .get_or_insert_with(|| create::create_instance(HL_API_VERSION))
            .clone();
        s.device = Some(create::create_device(&inst));
        s.device_token()
    });
    unsafe { *p_device = token };
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyDevice(_device: *mut c_void, _p_allocator: *const c_void) {
    with(|s| s.device = None);
}

#[no_mangle]
pub extern "C" fn vkGetDeviceQueue(
    _device: *mut c_void,
    _queue_family_index: u32,
    _queue_index: u32,
    p_queue: *mut *mut c_void,
) {
    if p_queue.is_null() {
        return;
    }
    let q = with(|s| s.queue_token());
    unsafe { *p_queue = q };
}

/// `vkGetDeviceQueue2` (Vulkan 1.1) — the `VkDeviceQueueInfo2`-parameterized retrieval. The device
/// exposes exactly one queue (family 0, index 0), so this returns the same lone queue token as
/// `vkGetDeviceQueue`; a request for any other `(family, index)` returns `VK_NULL_HANDLE`.
#[no_mangle]
pub extern "C" fn vkGetDeviceQueue2(
    _device: *mut c_void,
    p_queue_info: *const c_void,
    p_queue: *mut *mut c_void,
) {
    if p_queue.is_null() {
        return;
    }
    unsafe { *p_queue = core::ptr::null_mut() };
    let Some(info) = (unsafe { (p_queue_info as *const VkDeviceQueueInfo2).as_ref() }) else {
        return;
    };
    if info.queue_family_index != 0 || info.queue_index != 0 {
        return; // only the single (family 0, index 0) queue exists.
    }
    let q = with(|s| s.queue_token());
    unsafe { *p_queue = q };
}
