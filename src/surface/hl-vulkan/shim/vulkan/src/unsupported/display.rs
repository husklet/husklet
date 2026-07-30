use super::*;

pub extern "C" fn vkAcquireDrmDisplayEXT(
    physicalDevice: *mut core::ffi::c_void,
    drmFd: i32,
    display: u64,
) -> i32 {
    let _ = physicalDevice;
    let _ = drmFd;
    let _ = display;
    crate::stub::Call::unsupported("vkAcquireDrmDisplayEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkAcquireFullScreenExclusiveModeEXT(
    device: *mut core::ffi::c_void,
    swapchain: u64,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    crate::stub::Call::unsupported(
        "vkAcquireFullScreenExclusiveModeEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkAcquireWinrtDisplayNV(
    physicalDevice: *mut core::ffi::c_void,
    display: u64,
) -> i32 {
    let _ = physicalDevice;
    let _ = display;
    crate::stub::Call::unsupported("vkAcquireWinrtDisplayNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkAcquireXlibDisplayEXT(
    physicalDevice: *mut core::ffi::c_void,
    dpy: *mut core::ffi::c_void,
    display: u64,
) -> i32 {
    let _ = physicalDevice;
    let _ = dpy;
    let _ = display;
    crate::stub::Call::unsupported("vkAcquireXlibDisplayEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCreateDisplayModeKHR(
    physicalDevice: *mut core::ffi::c_void,
    display: u64,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pMode: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = display;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pMode;
    unsafe {
        if !pMode.is_null() {
            *(pMode as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateDisplayModeKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCreateDisplayPlaneSurfaceKHR(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateDisplayPlaneSurfaceKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCreateScreenSurfaceQNX(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateScreenSurfaceQNX", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkDisplayPowerControlEXT(
    device: *mut core::ffi::c_void,
    display: u64,
    pDisplayPowerInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = display;
    let _ = pDisplayPowerInfo;
    crate::stub::Call::unsupported("vkDisplayPowerControlEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDisplayModeProperties2KHR(
    physicalDevice: *mut core::ffi::c_void,
    display: u64,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = display;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported("vkGetDisplayModeProperties2KHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDisplayModePropertiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    display: u64,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = display;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported("vkGetDisplayModePropertiesKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDisplayPlaneCapabilities2KHR(
    physicalDevice: *mut core::ffi::c_void,
    pDisplayPlaneInfo: *const core::ffi::c_void,
    pCapabilities: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pDisplayPlaneInfo;
    let _ = pCapabilities;
    crate::stub::Call::unsupported(
        "vkGetDisplayPlaneCapabilities2KHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDisplayPlaneCapabilitiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    mode: u64,
    planeIndex: u32,
    pCapabilities: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = mode;
    let _ = planeIndex;
    let _ = pCapabilities;
    crate::stub::Call::unsupported(
        "vkGetDisplayPlaneCapabilitiesKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDisplayPlaneSupportedDisplaysKHR(
    physicalDevice: *mut core::ffi::c_void,
    planeIndex: u32,
    pDisplayCount: *mut core::ffi::c_void,
    pDisplays: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = planeIndex;
    let _ = pDisplayCount;
    let _ = pDisplays;
    crate::stub::Call::unsupported(
        "vkGetDisplayPlaneSupportedDisplaysKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDrmDisplayEXT(
    physicalDevice: *mut core::ffi::c_void,
    drmFd: i32,
    connectorId: u32,
    display: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = drmFd;
    let _ = connectorId;
    let _ = display;
    crate::stub::Call::unsupported("vkGetDrmDisplayEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetImageDrmFormatModifierPropertiesEXT(
    device: *mut core::ffi::c_void,
    image: u64,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = image;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetImageDrmFormatModifierPropertiesEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceDisplayPlaneProperties2KHR(
    physicalDevice: *mut core::ffi::c_void,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceDisplayPlaneProperties2KHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceDisplayPlanePropertiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceDisplayPlanePropertiesKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceDisplayProperties2KHR(
    physicalDevice: *mut core::ffi::c_void,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceDisplayProperties2KHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceDisplayPropertiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceDisplayPropertiesKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetRandROutputDisplayEXT(
    physicalDevice: *mut core::ffi::c_void,
    dpy: *mut core::ffi::c_void,
    rrOutput: u64,
    pDisplay: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = dpy;
    let _ = rrOutput;
    let _ = pDisplay;
    crate::stub::Call::unsupported("vkGetRandROutputDisplayEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetScreenBufferPropertiesQNX(
    device: *mut core::ffi::c_void,
    buffer: *const core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = buffer;
    let _ = pProperties;
    crate::stub::Call::unsupported("vkGetScreenBufferPropertiesQNX", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetWinrtDisplayNV(
    physicalDevice: *mut core::ffi::c_void,
    deviceRelativeId: u32,
    pDisplay: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = deviceRelativeId;
    let _ = pDisplay;
    crate::stub::Call::unsupported("vkGetWinrtDisplayNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkRegisterDisplayEventEXT(
    device: *mut core::ffi::c_void,
    display: u64,
    pDisplayEventInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pFence: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = display;
    let _ = pDisplayEventInfo;
    let _ = pAllocator;
    let _ = pFence;
    unsafe {
        if !pFence.is_null() {
            *(pFence as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkRegisterDisplayEventEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkReleaseDisplayEXT(physicalDevice: *mut core::ffi::c_void, display: u64) -> i32 {
    let _ = physicalDevice;
    let _ = display;
    crate::stub::Call::unsupported("vkReleaseDisplayEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkReleaseFullScreenExclusiveModeEXT(
    device: *mut core::ffi::c_void,
    swapchain: u64,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    crate::stub::Call::unsupported(
        "vkReleaseFullScreenExclusiveModeEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
