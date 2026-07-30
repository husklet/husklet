use super::*;

pub extern "C" fn vkCmdSetRayTracingPipelineStackSizeKHR(
    commandBuffer: *mut core::ffi::c_void,
    pipelineStackSize: u32,
) {
    let _ = commandBuffer;
    let _ = pipelineStackSize;
    crate::stub::Call::unsupported(
        "vkCmdSetRayTracingPipelineStackSizeKHR",
        "extension family not modeled",
    );
}

pub extern "C" fn vkCreateRayTracingPipelinesKHR(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pipelineCache: u64,
    createInfoCount: u32,
    pCreateInfos: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pPipelines: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pipelineCache;
    let _ = createInfoCount;
    let _ = pCreateInfos;
    let _ = pAllocator;
    let _ = pPipelines;
    unsafe {
        if !pPipelines.is_null() {
            *(pPipelines as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateRayTracingPipelinesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCreateRayTracingPipelinesNV(
    device: *mut core::ffi::c_void,
    pipelineCache: u64,
    createInfoCount: u32,
    pCreateInfos: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pPipelines: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pipelineCache;
    let _ = createInfoCount;
    let _ = pCreateInfos;
    let _ = pAllocator;
    let _ = pPipelines;
    unsafe {
        if !pPipelines.is_null() {
            *(pPipelines as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateRayTracingPipelinesNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetRayTracingCaptureReplayShaderGroupHandlesKHR(
    device: *mut core::ffi::c_void,
    pipeline: u64,
    firstGroup: u32,
    groupCount: u32,
    dataSize: usize,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pipeline;
    let _ = firstGroup;
    let _ = groupCount;
    let _ = dataSize;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetRayTracingCaptureReplayShaderGroupHandlesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetRayTracingShaderGroupHandlesKHR(
    device: *mut core::ffi::c_void,
    pipeline: u64,
    firstGroup: u32,
    groupCount: u32,
    dataSize: usize,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pipeline;
    let _ = firstGroup;
    let _ = groupCount;
    let _ = dataSize;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetRayTracingShaderGroupHandlesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetRayTracingShaderGroupHandlesNV(
    device: *mut core::ffi::c_void,
    pipeline: u64,
    firstGroup: u32,
    groupCount: u32,
    dataSize: usize,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pipeline;
    let _ = firstGroup;
    let _ = groupCount;
    let _ = dataSize;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetRayTracingShaderGroupHandlesNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetRayTracingShaderGroupStackSizeKHR(
    device: *mut core::ffi::c_void,
    pipeline: u64,
    group: u32,
    groupShader: i32,
) -> u64 {
    let _ = device;
    let _ = pipeline;
    let _ = group;
    let _ = groupShader;
    crate::stub::Call::unsupported(
        "vkGetRayTracingShaderGroupStackSizeKHR",
        "extension family not modeled",
    );
    0
}
