use super::*;

#[no_mangle]
pub extern "C" fn vkBindVideoSessionMemoryKHR(
    device: *mut core::ffi::c_void,
    videoSession: u64,
    bindSessionMemoryInfoCount: u32,
    pBindSessionMemoryInfos: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = videoSession;
    let _ = bindSessionMemoryInfoCount;
    let _ = pBindSessionMemoryInfos;
    crate::stub::Call::unsupported(
        "vkBindVideoSessionMemoryKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCmdBeginVideoCodingKHR(
    commandBuffer: *mut core::ffi::c_void,
    pBeginInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pBeginInfo;
    crate::stub::Call::unsupported("vkCmdBeginVideoCodingKHR", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdControlVideoCodingKHR(
    commandBuffer: *mut core::ffi::c_void,
    pCodingControlInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pCodingControlInfo;
    crate::stub::Call::unsupported("vkCmdControlVideoCodingKHR", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdDecodeVideoKHR(
    commandBuffer: *mut core::ffi::c_void,
    pDecodeInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pDecodeInfo;
    crate::stub::Call::unsupported("vkCmdDecodeVideoKHR", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdEncodeVideoKHR(
    commandBuffer: *mut core::ffi::c_void,
    pEncodeInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pEncodeInfo;
    crate::stub::Call::unsupported("vkCmdEncodeVideoKHR", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdEndVideoCodingKHR(
    commandBuffer: *mut core::ffi::c_void,
    pEndCodingInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pEndCodingInfo;
    crate::stub::Call::unsupported("vkCmdEndVideoCodingKHR", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCreateVideoSessionKHR(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pVideoSession: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pVideoSession;
    unsafe {
        if !pVideoSession.is_null() {
            *(pVideoSession as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateVideoSessionKHR", "extension family not modeled");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateVideoSessionParametersKHR(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pVideoSessionParameters: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pVideoSessionParameters;
    unsafe {
        if !pVideoSessionParameters.is_null() {
            *(pVideoSessionParameters as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateVideoSessionParametersKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkDestroyVideoSessionKHR(
    device: *mut core::ffi::c_void,
    videoSession: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = videoSession;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyVideoSessionKHR", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkDestroyVideoSessionParametersKHR(
    device: *mut core::ffi::c_void,
    videoSessionParameters: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = videoSessionParameters;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroyVideoSessionParametersKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkGetEncodedVideoSessionParametersKHR(
    device: *mut core::ffi::c_void,
    pVideoSessionParametersInfo: *const core::ffi::c_void,
    pFeedbackInfo: *mut core::ffi::c_void,
    pDataSize: *mut core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pVideoSessionParametersInfo;
    let _ = pFeedbackInfo;
    let _ = pDataSize;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetEncodedVideoSessionParametersKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoCapabilitiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pVideoProfile: *const core::ffi::c_void,
    pCapabilities: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pVideoProfile;
    let _ = pCapabilities;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceVideoCapabilitiesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pQualityLevelInfo: *const core::ffi::c_void,
    pQualityLevelProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pQualityLevelInfo;
    let _ = pQualityLevelProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoFormatPropertiesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pVideoFormatInfo: *const core::ffi::c_void,
    pVideoFormatPropertyCount: *mut core::ffi::c_void,
    pVideoFormatProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pVideoFormatInfo;
    let _ = pVideoFormatPropertyCount;
    let _ = pVideoFormatProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceVideoFormatPropertiesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetVideoSessionMemoryRequirementsKHR(
    device: *mut core::ffi::c_void,
    videoSession: u64,
    pMemoryRequirementsCount: *mut core::ffi::c_void,
    pMemoryRequirements: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = videoSession;
    let _ = pMemoryRequirementsCount;
    let _ = pMemoryRequirements;
    crate::stub::Call::unsupported(
        "vkGetVideoSessionMemoryRequirementsKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkUpdateVideoSessionParametersKHR(
    device: *mut core::ffi::c_void,
    videoSessionParameters: u64,
    pUpdateInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = videoSessionParameters;
    let _ = pUpdateInfo;
    crate::stub::Call::unsupported(
        "vkUpdateVideoSessionParametersKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
