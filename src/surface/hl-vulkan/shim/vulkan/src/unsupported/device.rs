use super::*;

pub extern "C" fn vkCompileDeferredNV(
    device: *mut core::ffi::c_void,
    pipeline: u64,
    shader: u32,
) -> i32 {
    let _ = device;
    let _ = pipeline;
    let _ = shader;
    crate::stub::Call::unsupported("vkCompileDeferredNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkDeferredOperationJoinKHR(
    device: *mut core::ffi::c_void,
    operation: u64,
) -> i32 {
    let _ = device;
    let _ = operation;
    crate::stub::Call::unsupported("vkDeferredOperationJoinKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetCommandPoolMemoryConsumption(
    device: *mut core::ffi::c_void,
    commandPool: u64,
    commandBuffer: *mut core::ffi::c_void,
    pConsumption: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = commandPool;
    let _ = commandBuffer;
    let _ = pConsumption;
    crate::stub::Call::unsupported(
        "vkGetCommandPoolMemoryConsumption",
        "extension not advertised",
    );
}

pub extern "C" fn vkGetCudaModuleCacheNV(
    device: *mut core::ffi::c_void,
    module: u64,
    pCacheSize: *mut core::ffi::c_void,
    pCacheData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = module;
    let _ = pCacheSize;
    let _ = pCacheData;
    crate::stub::Call::unsupported("vkGetCudaModuleCacheNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDeferredOperationMaxConcurrencyKHR(
    device: *mut core::ffi::c_void,
    operation: u64,
) -> u32 {
    let _ = device;
    let _ = operation;
    crate::stub::Call::unsupported(
        "vkGetDeferredOperationMaxConcurrencyKHR",
        "extension not advertised",
    );
    0
}

pub extern "C" fn vkGetDeferredOperationResultKHR(
    device: *mut core::ffi::c_void,
    operation: u64,
) -> i32 {
    let _ = device;
    let _ = operation;
    crate::stub::Call::unsupported(
        "vkGetDeferredOperationResultKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDeviceSubpassShadingMaxWorkgroupSizeHUAWEI(
    device: *mut core::ffi::c_void,
    renderpass: u64,
    pMaxWorkgroupSize: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = renderpass;
    let _ = pMaxWorkgroupSize;
    crate::stub::Call::unsupported(
        "vkGetDeviceSubpassShadingMaxWorkgroupSizeHUAWEI",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDynamicRenderingTilePropertiesQCOM(
    device: *mut core::ffi::c_void,
    pRenderingInfo: *const core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pRenderingInfo;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetDynamicRenderingTilePropertiesQCOM",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetFramebufferTilePropertiesQCOM(
    device: *mut core::ffi::c_void,
    framebuffer: u64,
    pPropertiesCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = framebuffer;
    let _ = pPropertiesCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetFramebufferTilePropertiesQCOM",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetGeneratedCommandsMemoryRequirementsNV(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pMemoryRequirements: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pInfo;
    let _ = pMemoryRequirements;
    crate::stub::Call::unsupported(
        "vkGetGeneratedCommandsMemoryRequirementsNV",
        "extension not advertised",
    );
}

pub extern "C" fn vkGetImageViewAddressNVX(
    device: *mut core::ffi::c_void,
    imageView: u64,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = imageView;
    let _ = pProperties;
    crate::stub::Call::unsupported("vkGetImageViewAddressNVX", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetImageViewHandleNVX(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) -> u32 {
    let _ = device;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkGetImageViewHandleNVX", "extension not advertised");
    0
}

pub extern "C" fn vkGetMemoryHostPointerPropertiesEXT(
    device: *mut core::ffi::c_void,
    handleType: i32,
    pHostPointer: *const core::ffi::c_void,
    pMemoryHostPointerProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = handleType;
    let _ = pHostPointer;
    let _ = pMemoryHostPointerProperties;
    crate::stub::Call::unsupported(
        "vkGetMemoryHostPointerPropertiesEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemoryRemoteAddressNV(
    device: *mut core::ffi::c_void,
    pMemoryGetRemoteAddressInfo: *const core::ffi::c_void,
    pAddress: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pMemoryGetRemoteAddressInfo;
    let _ = pAddress;
    crate::stub::Call::unsupported("vkGetMemoryRemoteAddressNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceExternalImageFormatPropertiesNV(
    physicalDevice: *mut core::ffi::c_void,
    format: i32,
    type_: i32,
    tiling: i32,
    usage: u32,
    flags: u32,
    externalHandleType: u32,
    pExternalImageFormatProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = format;
    let _ = type_;
    let _ = tiling;
    let _ = usage;
    let _ = flags;
    let _ = externalHandleType;
    let _ = pExternalImageFormatProperties;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceExternalImageFormatPropertiesNV",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceFragmentShadingRatesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pFragmentShadingRateCount: *mut core::ffi::c_void,
    pFragmentShadingRates: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pFragmentShadingRateCount;
    let _ = pFragmentShadingRates;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceFragmentShadingRatesKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceRefreshableObjectTypesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pRefreshableObjectTypeCount: *mut core::ffi::c_void,
    pRefreshableObjectTypes: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pRefreshableObjectTypeCount;
    let _ = pRefreshableObjectTypes;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceRefreshableObjectTypesKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceSupportedFramebufferMixedSamplesCombinationsNV(
    physicalDevice: *mut core::ffi::c_void,
    pCombinationCount: *mut core::ffi::c_void,
    pCombinations: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pCombinationCount;
    let _ = pCombinations;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceSupportedFramebufferMixedSamplesCombinationsNV",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetValidationCacheDataEXT(
    device: *mut core::ffi::c_void,
    validationCache: u64,
    pDataSize: *mut core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = validationCache;
    let _ = pDataSize;
    let _ = pData;
    crate::stub::Call::unsupported("vkGetValidationCacheDataEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkMergeValidationCachesEXT(
    device: *mut core::ffi::c_void,
    dstCache: u64,
    srcCacheCount: u32,
    pSrcCaches: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = dstCache;
    let _ = srcCacheCount;
    let _ = pSrcCaches;
    crate::stub::Call::unsupported("vkMergeValidationCachesEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkQueueNotifyOutOfBandNV(
    queue: *mut core::ffi::c_void,
    pQueueTypeInfo: *const core::ffi::c_void,
) {
    let _ = queue;
    let _ = pQueueTypeInfo;
    crate::stub::Call::unsupported("vkQueueNotifyOutOfBandNV", "extension not advertised");
}

pub extern "C" fn vkRegisterDeviceEventEXT(
    device: *mut core::ffi::c_void,
    pDeviceEventInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pFence: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pDeviceEventInfo;
    let _ = pAllocator;
    let _ = pFence;
    unsafe {
        if !pFence.is_null() {
            *(pFence as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkRegisterDeviceEventEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkSetDeviceMemoryPriorityEXT(
    device: *mut core::ffi::c_void,
    memory: u64,
    priority: f32,
) {
    let _ = device;
    let _ = memory;
    let _ = priority;
    crate::stub::Call::unsupported("vkSetDeviceMemoryPriorityEXT", "extension not advertised");
}

pub extern "C" fn vkSetHdrMetadataEXT(
    device: *mut core::ffi::c_void,
    swapchainCount: u32,
    pSwapchains: *const core::ffi::c_void,
    pMetadata: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = swapchainCount;
    let _ = pSwapchains;
    let _ = pMetadata;
    crate::stub::Call::unsupported("vkSetHdrMetadataEXT", "extension not advertised");
}

pub extern "C" fn vkSetLocalDimmingAMD(
    device: *mut core::ffi::c_void,
    swapChain: u64,
    localDimmingEnable: u32,
) {
    let _ = device;
    let _ = swapChain;
    let _ = localDimmingEnable;
    crate::stub::Call::unsupported("vkSetLocalDimmingAMD", "extension not advertised");
}
