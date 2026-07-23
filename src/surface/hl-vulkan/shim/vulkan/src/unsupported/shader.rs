use super::*;

#[no_mangle]
pub extern "C" fn vkCmdBindPipelineShaderGroupNV(
    commandBuffer: *mut core::ffi::c_void,
    pipelineBindPoint: i32,
    pipeline: u64,
    groupIndex: u32,
) {
    let _ = commandBuffer;
    let _ = pipelineBindPoint;
    let _ = pipeline;
    let _ = groupIndex;
    crate::stub::Call::unsupported("vkCmdBindPipelineShaderGroupNV", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdBindShadersEXT(
    commandBuffer: *mut core::ffi::c_void,
    stageCount: u32,
    pStages: *const core::ffi::c_void,
    pShaders: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = stageCount;
    let _ = pStages;
    let _ = pShaders;
    crate::stub::Call::unsupported("vkCmdBindShadersEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdUpdatePipelineIndirectBufferNV(
    commandBuffer: *mut core::ffi::c_void,
    pipelineBindPoint: i32,
    pipeline: u64,
) {
    let _ = commandBuffer;
    let _ = pipelineBindPoint;
    let _ = pipeline;
    crate::stub::Call::unsupported(
        "vkCmdUpdatePipelineIndirectBufferNV",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCreateExecutionGraphPipelinesAMDX(
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
        "vkCreateExecutionGraphPipelinesAMDX",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateShadersEXT(
    device: *mut core::ffi::c_void,
    createInfoCount: u32,
    pCreateInfos: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pShaders: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = createInfoCount;
    let _ = pCreateInfos;
    let _ = pAllocator;
    let _ = pShaders;
    unsafe {
        if !pShaders.is_null() {
            *(pShaders as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateShadersEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkDestroyShaderEXT(
    device: *mut core::ffi::c_void,
    shader: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = shader;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyShaderEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkGetExecutionGraphPipelineNodeIndexAMDX(
    device: *mut core::ffi::c_void,
    executionGraph: u64,
    pNodeInfo: *const core::ffi::c_void,
    pNodeIndex: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = executionGraph;
    let _ = pNodeInfo;
    let _ = pNodeIndex;
    crate::stub::Call::unsupported(
        "vkGetExecutionGraphPipelineNodeIndexAMDX",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetExecutionGraphPipelineScratchSizeAMDX(
    device: *mut core::ffi::c_void,
    executionGraph: u64,
    pSizeInfo: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = executionGraph;
    let _ = pSizeInfo;
    crate::stub::Call::unsupported(
        "vkGetExecutionGraphPipelineScratchSizeAMDX",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutableInternalRepresentationsKHR(
    device: *mut core::ffi::c_void,
    pExecutableInfo: *const core::ffi::c_void,
    pInternalRepresentationCount: *mut core::ffi::c_void,
    pInternalRepresentations: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pExecutableInfo;
    let _ = pInternalRepresentationCount;
    let _ = pInternalRepresentations;
    crate::stub::Call::unsupported(
        "vkGetPipelineExecutableInternalRepresentationsKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutablePropertiesKHR(
    device: *mut core::ffi::c_void,
    pPipelineInfo: *const core::ffi::c_void,
    pExecutableCount: *mut core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pPipelineInfo;
    let _ = pExecutableCount;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetPipelineExecutablePropertiesKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutableStatisticsKHR(
    device: *mut core::ffi::c_void,
    pExecutableInfo: *const core::ffi::c_void,
    pStatisticCount: *mut core::ffi::c_void,
    pStatistics: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pExecutableInfo;
    let _ = pStatisticCount;
    let _ = pStatistics;
    crate::stub::Call::unsupported(
        "vkGetPipelineExecutableStatisticsKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPipelineIndirectDeviceAddressNV(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) -> u64 {
    let _ = device;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkGetPipelineIndirectDeviceAddressNV",
        "extension not advertised",
    );
    0
}

#[no_mangle]
pub extern "C" fn vkGetPipelineIndirectMemoryRequirementsNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pMemoryRequirements: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pMemoryRequirements;
    crate::stub::Call::unsupported(
        "vkGetPipelineIndirectMemoryRequirementsNV",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetPipelinePropertiesEXT(
    device: *mut core::ffi::c_void,
    pPipelineInfo: *const core::ffi::c_void,
    pPipelineProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pPipelineInfo;
    let _ = pPipelineProperties;
    crate::stub::Call::unsupported("vkGetPipelinePropertiesEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetShaderBinaryDataEXT(
    device: *mut core::ffi::c_void,
    shader: u64,
    pDataSize: *mut core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = shader;
    let _ = pDataSize;
    let _ = pData;
    crate::stub::Call::unsupported("vkGetShaderBinaryDataEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetShaderInfoAMD(
    device: *mut core::ffi::c_void,
    pipeline: u64,
    shaderStage: i32,
    infoType: i32,
    pInfoSize: *mut core::ffi::c_void,
    pInfo: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pipeline;
    let _ = shaderStage;
    let _ = infoType;
    let _ = pInfoSize;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkGetShaderInfoAMD", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetShaderModuleCreateInfoIdentifierEXT(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pIdentifier: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pIdentifier;
    crate::stub::Call::unsupported(
        "vkGetShaderModuleCreateInfoIdentifierEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetShaderModuleIdentifierEXT(
    device: *mut core::ffi::c_void,
    shaderModule: u64,
    pIdentifier: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = shaderModule;
    let _ = pIdentifier;
    crate::stub::Call::unsupported("vkGetShaderModuleIdentifierEXT", "extension not advertised");
}
