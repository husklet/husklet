use super::*;

pub extern "C" fn vkAcquirePerformanceConfigurationINTEL(
    device: *mut core::ffi::c_void,
    pAcquireInfo: *const core::ffi::c_void,
    pConfiguration: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pAcquireInfo;
    let _ = pConfiguration;
    crate::stub::Call::unsupported(
        "vkAcquirePerformanceConfigurationINTEL",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkAcquireProfilingLockKHR(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkAcquireProfilingLockKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCmdSetCheckpointNV(
    commandBuffer: *mut core::ffi::c_void,
    pCheckpointMarker: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pCheckpointMarker;
    crate::stub::Call::unsupported("vkCmdSetCheckpointNV", "extension not advertised");
}

pub extern "C" fn vkCmdSetPerformanceMarkerINTEL(
    commandBuffer: *mut core::ffi::c_void,
    pMarkerInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = commandBuffer;
    let _ = pMarkerInfo;
    crate::stub::Call::unsupported("vkCmdSetPerformanceMarkerINTEL", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCmdSetPerformanceOverrideINTEL(
    commandBuffer: *mut core::ffi::c_void,
    pOverrideInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = commandBuffer;
    let _ = pOverrideInfo;
    crate::stub::Call::unsupported(
        "vkCmdSetPerformanceOverrideINTEL",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCmdSetPerformanceStreamMarkerINTEL(
    commandBuffer: *mut core::ffi::c_void,
    pMarkerInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = commandBuffer;
    let _ = pMarkerInfo;
    crate::stub::Call::unsupported(
        "vkCmdSetPerformanceStreamMarkerINTEL",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCmdWriteBufferMarker2AMD(
    commandBuffer: *mut core::ffi::c_void,
    stage: u64,
    dstBuffer: u64,
    dstOffset: u64,
    marker: u32,
) {
    let _ = commandBuffer;
    let _ = stage;
    let _ = dstBuffer;
    let _ = dstOffset;
    let _ = marker;
    crate::stub::Call::unsupported("vkCmdWriteBufferMarker2AMD", "extension not advertised");
}

pub extern "C" fn vkCmdWriteBufferMarkerAMD(
    commandBuffer: *mut core::ffi::c_void,
    pipelineStage: i32,
    dstBuffer: u64,
    dstOffset: u64,
    marker: u32,
) {
    let _ = commandBuffer;
    let _ = pipelineStage;
    let _ = dstBuffer;
    let _ = dstOffset;
    let _ = marker;
    crate::stub::Call::unsupported("vkCmdWriteBufferMarkerAMD", "extension not advertised");
}

pub extern "C" fn vkEnumeratePhysicalDeviceQueueFamilyPerformanceQueryCountersKHR(
    physicalDevice: *mut core::ffi::c_void,
    queueFamilyIndex: u32,
    pCounterCount: *mut core::ffi::c_void,
    pCounters: *mut core::ffi::c_void,
    pCounterDescriptions: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = queueFamilyIndex;
    let _ = pCounterCount;
    let _ = pCounters;
    let _ = pCounterDescriptions;
    crate::stub::Call::unsupported(
        "vkEnumeratePhysicalDeviceQueueFamilyPerformanceQueryCountersKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetDeviceFaultInfoEXT(
    device: *mut core::ffi::c_void,
    pFaultCounts: *mut core::ffi::c_void,
    pFaultInfo: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pFaultCounts;
    let _ = pFaultInfo;
    crate::stub::Call::unsupported("vkGetDeviceFaultInfoEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetFaultData(
    device: *mut core::ffi::c_void,
    faultQueryBehavior: i32,
    pUnrecordedFaults: *mut core::ffi::c_void,
    pFaultCount: *mut core::ffi::c_void,
    pFaults: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = faultQueryBehavior;
    let _ = pUnrecordedFaults;
    let _ = pFaultCount;
    let _ = pFaults;
    crate::stub::Call::unsupported("vkGetFaultData", "extension not advertised");
    VK_ERROR_FEATURE_NOT_PRESENT
}

pub extern "C" fn vkGetLatencyTimingsNV(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    pLatencyMarkerInfo: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = swapchain;
    let _ = pLatencyMarkerInfo;
    crate::stub::Call::unsupported("vkGetLatencyTimingsNV", "extension not advertised");
}

pub extern "C" fn vkGetPerformanceParameterINTEL(
    device: *mut core::ffi::c_void,
    parameter: i32,
    pValue: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = parameter;
    let _ = pValue;
    crate::stub::Call::unsupported("vkGetPerformanceParameterINTEL", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetPhysicalDeviceQueueFamilyPerformanceQueryPassesKHR(
    physicalDevice: *mut core::ffi::c_void,
    pPerformanceQueryCreateInfo: *const core::ffi::c_void,
    pNumPasses: *mut core::ffi::c_void,
) {
    let _ = physicalDevice;
    let _ = pPerformanceQueryCreateInfo;
    let _ = pNumPasses;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceQueueFamilyPerformanceQueryPassesKHR",
        "extension not advertised",
    );
}

pub extern "C" fn vkGetQueueCheckpointData2NV(
    queue: *mut core::ffi::c_void,
    pCheckpointDataCount: *mut core::ffi::c_void,
    pCheckpointData: *mut core::ffi::c_void,
) {
    let _ = queue;
    let _ = pCheckpointDataCount;
    let _ = pCheckpointData;
    unsafe {
        if !pCheckpointDataCount.is_null() {
            *(pCheckpointDataCount as *mut u32) = 0;
        }
    }
    crate::stub::Call::unsupported("vkGetQueueCheckpointData2NV", "extension not advertised");
}

pub extern "C" fn vkGetQueueCheckpointDataNV(
    queue: *mut core::ffi::c_void,
    pCheckpointDataCount: *mut core::ffi::c_void,
    pCheckpointData: *mut core::ffi::c_void,
) {
    let _ = queue;
    let _ = pCheckpointDataCount;
    let _ = pCheckpointData;
    unsafe {
        if !pCheckpointDataCount.is_null() {
            *(pCheckpointDataCount as *mut u32) = 0;
        }
    }
    crate::stub::Call::unsupported("vkGetQueueCheckpointDataNV", "extension not advertised");
}

pub extern "C" fn vkGetRefreshCycleDurationGOOGLE(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    pDisplayTimingProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    let _ = pDisplayTimingProperties;
    crate::stub::Call::unsupported(
        "vkGetRefreshCycleDurationGOOGLE",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkInitializePerformanceApiINTEL(
    device: *mut core::ffi::c_void,
    pInitializeInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInitializeInfo;
    crate::stub::Call::unsupported(
        "vkInitializePerformanceApiINTEL",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkLatencySleepNV(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    pSleepInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    let _ = pSleepInfo;
    crate::stub::Call::unsupported("vkLatencySleepNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkQueueSetPerformanceConfigurationINTEL(
    queue: *mut core::ffi::c_void,
    configuration: u64,
) -> i32 {
    let _ = queue;
    let _ = configuration;
    crate::stub::Call::unsupported(
        "vkQueueSetPerformanceConfigurationINTEL",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkReleasePerformanceConfigurationINTEL(
    device: *mut core::ffi::c_void,
    configuration: u64,
) -> i32 {
    let _ = device;
    let _ = configuration;
    crate::stub::Call::unsupported(
        "vkReleasePerformanceConfigurationINTEL",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkReleaseProfilingLockKHR(device: *mut core::ffi::c_void) {
    let _ = device;
    crate::stub::Call::unsupported("vkReleaseProfilingLockKHR", "extension not advertised");
}

pub extern "C" fn vkSetLatencyMarkerNV(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    pLatencyMarkerInfo: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = swapchain;
    let _ = pLatencyMarkerInfo;
    crate::stub::Call::unsupported("vkSetLatencyMarkerNV", "extension not advertised");
}

pub extern "C" fn vkSetLatencySleepModeNV(
    device: *mut core::ffi::c_void,
    swapchain: u64,
    pSleepModeInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = swapchain;
    let _ = pSleepModeInfo;
    crate::stub::Call::unsupported("vkSetLatencySleepModeNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkUninitializePerformanceApiINTEL(device: *mut core::ffi::c_void) {
    let _ = device;
    crate::stub::Call::unsupported(
        "vkUninitializePerformanceApiINTEL",
        "extension not advertised",
    );
}
