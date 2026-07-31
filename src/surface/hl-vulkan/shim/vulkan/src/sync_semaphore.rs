//! Binary and timeline semaphore entry points.

use core::ffi::c_void;

use hl_vulkan::result::Status;
use hl_vulkan::service::sync;

use super::ShimState;
use crate::state::StateStore;
use crate::types::*;

struct ExtensionChain;

impl ExtensionChain {
    unsafe fn find(mut node: *const c_void, target: i32) -> *const c_void {
        while !node.is_null() {
            let base = &*(node as *const VkBaseInStructure);
            if base.s_type == target {
                return node;
            }
            node = base.p_next as *const c_void;
        }
        core::ptr::null()
    }
}

struct SemaphoreInfo;

impl SemaphoreInfo {
    fn parse_type(create_info: *const c_void) -> (bool, u64) {
        let Some(create_info) = (unsafe { (create_info as *const VkSemaphoreCreateInfo).as_ref() })
        else {
            return (false, 0);
        };
        let node = unsafe {
            ExtensionChain::find(
                create_info.p_next,
                VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO,
            )
        };
        if node.is_null() {
            return (false, 0);
        }
        let type_info = unsafe { &*(node as *const VkSemaphoreTypeCreateInfo) };
        (
            type_info.semaphore_type == VK_SEMAPHORE_TYPE_TIMELINE,
            type_info.initial_value,
        )
    }
}

pub extern "C" fn vkCreateSemaphore(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_semaphore: *mut u64,
) -> VkResult {
    let (timeline, initial) = SemaphoreInfo::parse_type(p_create_info);
    let handle = StateStore::with(|state| {
        let device = state.device_mut()?;
        Some(sync::create_semaphore(device, timeline, initial))
    });
    match handle {
        Some(handle) => {
            if !p_semaphore.is_null() {
                unsafe { *p_semaphore = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

pub extern "C" fn vkDestroySemaphore(
    _device: *mut c_void,
    semaphore: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|state| {
        if let Some(device) = state.device_mut() {
            device.destroy_semaphore(semaphore);
        }
    });
}

pub extern "C" fn vkSignalSemaphore(
    _device: *mut c_void,
    p_signal_info: *const c_void,
) -> VkResult {
    let Some(info) = (unsafe { (p_signal_info as *const VkSemaphoreSignalInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    ShimState::with_device_result(|device| {
        match device.signal_semaphore(info.semaphore, info.value) {
            Ok(()) => VK_SUCCESS,
            Err(error) => Status::from_error(&error),
        }
    })
}

pub extern "C" fn vkGetSemaphoreCounterValue(
    _device: *mut c_void,
    semaphore: u64,
    p_value: *mut u64,
) -> VkResult {
    ShimState::with_device_result(|device| match device.semaphore_counter(semaphore) {
        Ok(value) => {
            if let Some(output) = unsafe { p_value.as_mut() } {
                *output = value;
            }
            VK_SUCCESS
        }
        Err(error) => Status::from_error(&error),
    })
}

pub extern "C" fn vkWaitSemaphores(
    _device: *mut c_void,
    p_wait_info: *const c_void,
    _timeout: u64,
) -> VkResult {
    let Some(info) = (unsafe { (p_wait_info as *const VkSemaphoreWaitInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.semaphore_count == 0 || info.p_semaphores.is_null() || info.p_values.is_null() {
        return VK_SUCCESS;
    }
    let semaphores =
        unsafe { std::slice::from_raw_parts(info.p_semaphores, info.semaphore_count as usize) };
    let values =
        unsafe { std::slice::from_raw_parts(info.p_values, info.semaphore_count as usize) };
    let any = info.flags & VK_SEMAPHORE_WAIT_ANY_BIT != 0;
    ShimState::with_device_result(|device| {
        if sync::wait_semaphores(device, semaphores, values, any) {
            VK_SUCCESS
        } else {
            VK_TIMEOUT
        }
    })
}

pub extern "C" fn vkGetSemaphoreCounterValueKHR(
    device: *mut c_void,
    semaphore: u64,
    p_value: *mut u64,
) -> VkResult {
    vkGetSemaphoreCounterValue(device, semaphore, p_value)
}

pub extern "C" fn vkSignalSemaphoreKHR(
    device: *mut c_void,
    p_signal_info: *const c_void,
) -> VkResult {
    vkSignalSemaphore(device, p_signal_info)
}

pub extern "C" fn vkWaitSemaphoresKHR(
    device: *mut c_void,
    p_wait_info: *const c_void,
    timeout: u64,
) -> VkResult {
    vkWaitSemaphores(device, p_wait_info, timeout)
}
