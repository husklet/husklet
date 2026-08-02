use super::*;

// ==================================================================================================
// fences
// ==================================================================================================

pub extern "C" fn vkCreateFence(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_fence: *mut u64,
) -> VkResult {
    let signaled = unsafe {
        (p_create_info as *const VkFenceCreateInfo)
            .as_ref()
            .map(|ci| ci.flags & 0x1 != 0)
            .unwrap_or(false)
    };
    if !p_fence.is_null() {
        unsafe { *p_fence = 0 };
    }
    ShimState::with_sink(
        |dev, sink| match create::create_fence(dev, sink, signaled) {
            Ok(h) => {
                if !p_fence.is_null() {
                    unsafe { *p_fence = h };
                }
                VK_SUCCESS
            }
            Err(e) => Status::from_error(&e),
        },
    )
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkDestroyFence(_device: *mut c_void, fence: u64, _p_allocator: *const c_void) {
    ShimState::with_sink(|dev, sink| {
        let _ = create::destroy_fence(dev, sink, fence);
    });
}

pub extern "C" fn vkWaitForFences(
    _device: *mut c_void,
    fence_count: u32,
    p_fences: *const u64,
    _wait_all: u32,
    _timeout: u64,
) -> VkResult {
    if p_fences.is_null() {
        return VK_SUCCESS;
    }
    let fences = unsafe { std::slice::from_raw_parts(p_fences, fence_count as usize) };
    ShimState::with_sink(|dev, sink| {
        for &f in fences {
            if let Err(e) = Device::wait_for_fence(dev, sink, f) {
                return Status::from_error(&e);
            }
        }
        VK_SUCCESS
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkResetFences(
    _device: *mut c_void,
    fence_count: u32,
    p_fences: *const u64,
) -> VkResult {
    if p_fences.is_null() {
        return VK_SUCCESS;
    }
    let fences = unsafe { std::slice::from_raw_parts(p_fences, fence_count as usize) };
    ShimState::with_sink(|dev, _| {
        for &f in fences {
            let _ = dev.reset_fence(f);
        }
        VK_SUCCESS
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

/// `vkGetFenceStatus` — poll the fence's guest-side signaled state (`VK_SUCCESS` when signaled,
/// `VK_NOT_READY` otherwise). Non-blocking, unlike `vkWaitForFences`. Errors on an unknown fence.
pub extern "C" fn vkGetFenceStatus(_device: *mut c_void, fence: u64) -> VkResult {
    ShimState::with_sink(|dev, _| match dev.is_fence_signaled(fence) {
        Ok(true) => VK_SUCCESS,
        Ok(false) => VK_NOT_READY,
        Err(e) => Status::from_error(&e),
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}
