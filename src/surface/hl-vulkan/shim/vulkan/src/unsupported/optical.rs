use super::*;

pub extern "C" fn vkBindOpticalFlowSessionImageNV(
    device: *mut core::ffi::c_void,
    session: u64,
    bindingPoint: i32,
    view: u64,
    layout: i32,
) -> i32 {
    let _ = device;
    let _ = session;
    let _ = bindingPoint;
    let _ = view;
    let _ = layout;
    crate::stub::Call::unsupported(
        "vkBindOpticalFlowSessionImageNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCmdOpticalFlowExecuteNV(
    commandBuffer: *mut core::ffi::c_void,
    session: u64,
    pExecuteInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = session;
    let _ = pExecuteInfo;
    crate::stub::Call::unsupported("vkCmdOpticalFlowExecuteNV", "extension family not modeled");
}

pub extern "C" fn vkCreateOpticalFlowSessionNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSession: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSession;
    unsafe {
        if !pSession.is_null() {
            *(pSession as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateOpticalFlowSessionNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkDestroyOpticalFlowSessionNV(
    device: *mut core::ffi::c_void,
    session: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = session;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroyOpticalFlowSessionNV",
        "extension family not modeled",
    );
}

pub extern "C" fn vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceCooperativeMatrixPropertiesNV(
    physicalDevice: *mut core::ffi::c_void,
    pPropertyCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pPropertyCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceCooperativeMatrixPropertiesNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceOpticalFlowImageFormatsNV(
    physicalDevice: *mut core::ffi::c_void,
    pOpticalFlowImageFormatInfo: *const core::ffi::c_void,
    pFormatCount: *mut core::ffi::c_void,
    pImageFormatProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pOpticalFlowImageFormatInfo;
    let _ = pFormatCount;
    let _ = pImageFormatProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceOpticalFlowImageFormatsNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
