use super::*;

pub extern "C" fn vkAcquireImageANDROID(
    device: *mut core::ffi::c_void,
    image: u64,
    nativeFenceFd: i32,
    semaphore: u64,
    fence: u64,
) -> i32 {
    let _ = device;
    let _ = image;
    let _ = nativeFenceFd;
    let _ = semaphore;
    let _ = fence;
    crate::stub::Call::unsupported("vkAcquireImageANDROID", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCreateSharedSwapchainsKHR(
    device: *mut core::ffi::c_void,
    swapchainCount: u32,
    pCreateInfos: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSwapchains: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = swapchainCount;
    let _ = pCreateInfos;
    let _ = pAllocator;
    let _ = pSwapchains;
    unsafe {
        if !pSwapchains.is_null() {
            *(pSwapchains as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateSharedSwapchainsKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPastPresentationTimingGOOGLE(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    pPresentationTimingCount: *mut core::ffi::c_void,
    pPresentationTimings: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    let _ = pPresentationTimingCount;
    let _ = pPresentationTimings;
    crate::stub::Call::unsupported(
        "vkGetPastPresentationTimingGOOGLE",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceDirectFBPresentationSupportEXT(
    physicalDevice: *mut core::ffi::c_void,
    queueFamilyIndex: u32,
    dfb: *mut core::ffi::c_void,
) -> u32 {
    let _ = physicalDevice;
    let _ = queueFamilyIndex;
    let _ = dfb;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceDirectFBPresentationSupportEXT",
        "extension not advertised",
    );
    0
}

pub extern "C" fn vkGetPhysicalDeviceScreenPresentationSupportQNX(
    physicalDevice: *mut core::ffi::c_void,
    queueFamilyIndex: u32,
    window: *mut core::ffi::c_void,
) -> u32 {
    let _ = physicalDevice;
    let _ = queueFamilyIndex;
    let _ = window;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceScreenPresentationSupportQNX",
        "extension not advertised",
    );
    0
}

pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModes2EXT(
    physicalDevice: *mut core::ffi::c_void,
    pSurfaceInfo: *const core::ffi::c_void,
    pPresentModeCount: *mut core::ffi::c_void,
    pPresentModes: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pSurfaceInfo;
    let _ = pPresentModeCount;
    let _ = pPresentModes;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceSurfacePresentModes2EXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceWin32PresentationSupportKHR(
    physicalDevice: *mut core::ffi::c_void,
    queueFamilyIndex: u32,
) -> u32 {
    let _ = physicalDevice;
    let _ = queueFamilyIndex;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceWin32PresentationSupportKHR",
        "extension not advertised",
    );
    0
}

pub extern "C" fn vkGetSwapchainCounterEXT(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    counter: i32,
    pCounterValue: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    let _ = counter;
    let _ = pCounterValue;
    crate::stub::Call::unsupported("vkGetSwapchainCounterEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetSwapchainGrallocUsage2ANDROID(
    device: *mut core::ffi::c_void,
    format: i32,
    imageUsage: u32,
    swapchainImageUsage: u32,
    grallocConsumerUsage: *mut core::ffi::c_void,
    grallocProducerUsage: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = format;
    let _ = imageUsage;
    let _ = swapchainImageUsage;
    let _ = grallocConsumerUsage;
    let _ = grallocProducerUsage;
    crate::stub::Call::unsupported(
        "vkGetSwapchainGrallocUsage2ANDROID",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetSwapchainGrallocUsageANDROID(
    device: *mut core::ffi::c_void,
    format: i32,
    imageUsage: u32,
    grallocUsage: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = format;
    let _ = imageUsage;
    let _ = grallocUsage;
    crate::stub::Call::unsupported(
        "vkGetSwapchainGrallocUsageANDROID",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetSwapchainStatusKHR(device: *mut core::ffi::c_void, swapchain: u64) -> i32 {
    let _ = device;
    let _ = swapchain;
    crate::stub::Call::unsupported("vkGetSwapchainStatusKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkQueueSignalReleaseImageANDROID(
    queue: *mut core::ffi::c_void,
    waitSemaphoreCount: u32,
    pWaitSemaphores: *const core::ffi::c_void,
    image: u64,
    pNativeFenceFd: *mut core::ffi::c_void,
) -> i32 {
    let _ = queue;
    let _ = waitSemaphoreCount;
    let _ = pWaitSemaphores;
    let _ = image;
    let _ = pNativeFenceFd;
    crate::stub::Call::unsupported(
        "vkQueueSignalReleaseImageANDROID",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkReleaseSwapchainImagesEXT(
    device: *mut core::ffi::c_void,
    pReleaseInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pReleaseInfo;
    crate::stub::Call::unsupported("vkReleaseSwapchainImagesEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkWaitForPresentKHR(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    presentId: u64,
    timeout: u64,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    let _ = presentId;
    let _ = timeout;
    crate::stub::Call::unsupported("vkWaitForPresentKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}
