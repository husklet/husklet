pub extern "C" fn vkCmdTraceRaysIndirect2KHR(
    commandBuffer: *mut core::ffi::c_void,
    indirectDeviceAddress: u64,
) {
    let _ = commandBuffer;
    let _ = indirectDeviceAddress;
    crate::stub::Call::unsupported("vkCmdTraceRaysIndirect2KHR", "extension family not modeled");
}

pub extern "C" fn vkCmdTraceRaysIndirectKHR(
    commandBuffer: *mut core::ffi::c_void,
    pRaygenShaderBindingTable: *const core::ffi::c_void,
    pMissShaderBindingTable: *const core::ffi::c_void,
    pHitShaderBindingTable: *const core::ffi::c_void,
    pCallableShaderBindingTable: *const core::ffi::c_void,
    indirectDeviceAddress: u64,
) {
    let _ = commandBuffer;
    let _ = pRaygenShaderBindingTable;
    let _ = pMissShaderBindingTable;
    let _ = pHitShaderBindingTable;
    let _ = pCallableShaderBindingTable;
    let _ = indirectDeviceAddress;
    crate::stub::Call::unsupported("vkCmdTraceRaysIndirectKHR", "extension family not modeled");
}

pub extern "C" fn vkCmdTraceRaysKHR(
    commandBuffer: *mut core::ffi::c_void,
    pRaygenShaderBindingTable: *const core::ffi::c_void,
    pMissShaderBindingTable: *const core::ffi::c_void,
    pHitShaderBindingTable: *const core::ffi::c_void,
    pCallableShaderBindingTable: *const core::ffi::c_void,
    width: u32,
    height: u32,
    depth: u32,
) {
    let _ = commandBuffer;
    let _ = pRaygenShaderBindingTable;
    let _ = pMissShaderBindingTable;
    let _ = pHitShaderBindingTable;
    let _ = pCallableShaderBindingTable;
    let _ = width;
    let _ = height;
    let _ = depth;
    crate::stub::Call::unsupported("vkCmdTraceRaysKHR", "extension family not modeled");
}

pub extern "C" fn vkCmdTraceRaysNV(
    commandBuffer: *mut core::ffi::c_void,
    raygenShaderBindingTableBuffer: u64,
    raygenShaderBindingOffset: u64,
    missShaderBindingTableBuffer: u64,
    missShaderBindingOffset: u64,
    missShaderBindingStride: u64,
    hitShaderBindingTableBuffer: u64,
    hitShaderBindingOffset: u64,
    hitShaderBindingStride: u64,
    callableShaderBindingTableBuffer: u64,
    callableShaderBindingOffset: u64,
    callableShaderBindingStride: u64,
    width: u32,
    height: u32,
    depth: u32,
) {
    let _ = commandBuffer;
    let _ = raygenShaderBindingTableBuffer;
    let _ = raygenShaderBindingOffset;
    let _ = missShaderBindingTableBuffer;
    let _ = missShaderBindingOffset;
    let _ = missShaderBindingStride;
    let _ = hitShaderBindingTableBuffer;
    let _ = hitShaderBindingOffset;
    let _ = hitShaderBindingStride;
    let _ = callableShaderBindingTableBuffer;
    let _ = callableShaderBindingOffset;
    let _ = callableShaderBindingStride;
    let _ = width;
    let _ = height;
    let _ = depth;
    crate::stub::Call::unsupported("vkCmdTraceRaysNV", "extension family not modeled");
}

pub extern "C" fn vkCmdCopyMemoryIndirectNV(
    commandBuffer: *mut core::ffi::c_void,
    copyBufferAddress: u64,
    copyCount: u32,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = copyBufferAddress;
    let _ = copyCount;
    let _ = stride;
    crate::stub::Call::unsupported("vkCmdCopyMemoryIndirectNV", "extension not advertised");
}

pub extern "C" fn vkCmdCopyMemoryToImageIndirectNV(
    commandBuffer: *mut core::ffi::c_void,
    copyBufferAddress: u64,
    copyCount: u32,
    stride: u32,
    dstImage: u64,
    dstImageLayout: i32,
    pImageSubresources: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = copyBufferAddress;
    let _ = copyCount;
    let _ = stride;
    let _ = dstImage;
    let _ = dstImageLayout;
    let _ = pImageSubresources;
    crate::stub::Call::unsupported(
        "vkCmdCopyMemoryToImageIndirectNV",
        "extension not advertised",
    );
}

pub extern "C" fn vkCmdCuLaunchKernelNVX(
    commandBuffer: *mut core::ffi::c_void,
    pLaunchInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pLaunchInfo;
    crate::stub::Call::unsupported("vkCmdCuLaunchKernelNVX", "extension not advertised");
}

pub extern "C" fn vkCmdCudaLaunchKernelNV(
    commandBuffer: *mut core::ffi::c_void,
    pLaunchInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pLaunchInfo;
    crate::stub::Call::unsupported("vkCmdCudaLaunchKernelNV", "extension not advertised");
}

pub extern "C" fn vkCmdDecompressMemoryIndirectCountNV(
    commandBuffer: *mut core::ffi::c_void,
    indirectCommandsAddress: u64,
    indirectCommandsCountAddress: u64,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = indirectCommandsAddress;
    let _ = indirectCommandsCountAddress;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkCmdDecompressMemoryIndirectCountNV",
        "extension not advertised",
    );
}

pub extern "C" fn vkCmdDecompressMemoryNV(
    commandBuffer: *mut core::ffi::c_void,
    decompressRegionCount: u32,
    pDecompressMemoryRegions: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = decompressRegionCount;
    let _ = pDecompressMemoryRegions;
    crate::stub::Call::unsupported("vkCmdDecompressMemoryNV", "extension not advertised");
}

pub extern "C" fn vkCmdDispatchGraphAMDX(
    commandBuffer: *mut core::ffi::c_void,
    scratch: u64,
    pCountInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = scratch;
    let _ = pCountInfo;
    crate::stub::Call::unsupported("vkCmdDispatchGraphAMDX", "extension not advertised");
}

pub extern "C" fn vkCmdDispatchGraphIndirectAMDX(
    commandBuffer: *mut core::ffi::c_void,
    scratch: u64,
    pCountInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = scratch;
    let _ = pCountInfo;
    crate::stub::Call::unsupported("vkCmdDispatchGraphIndirectAMDX", "extension not advertised");
}

pub extern "C" fn vkCmdDispatchGraphIndirectCountAMDX(
    commandBuffer: *mut core::ffi::c_void,
    scratch: u64,
    countInfo: u64,
) {
    let _ = commandBuffer;
    let _ = scratch;
    let _ = countInfo;
    crate::stub::Call::unsupported(
        "vkCmdDispatchGraphIndirectCountAMDX",
        "extension not advertised",
    );
}

pub extern "C" fn vkCmdExecuteGeneratedCommandsNV(
    commandBuffer: *mut core::ffi::c_void,
    isPreprocessed: u32,
    pGeneratedCommandsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = isPreprocessed;
    let _ = pGeneratedCommandsInfo;
    crate::stub::Call::unsupported(
        "vkCmdExecuteGeneratedCommandsNV",
        "extension not advertised",
    );
}

pub extern "C" fn vkCmdInitializeGraphScratchMemoryAMDX(
    commandBuffer: *mut core::ffi::c_void,
    scratch: u64,
) {
    let _ = commandBuffer;
    let _ = scratch;
    crate::stub::Call::unsupported(
        "vkCmdInitializeGraphScratchMemoryAMDX",
        "extension not advertised",
    );
}

pub extern "C" fn vkCmdPreprocessGeneratedCommandsNV(
    commandBuffer: *mut core::ffi::c_void,
    pGeneratedCommandsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pGeneratedCommandsInfo;
    crate::stub::Call::unsupported(
        "vkCmdPreprocessGeneratedCommandsNV",
        "extension not advertised",
    );
}

pub extern "C" fn vkCmdRefreshObjectsKHR(
    commandBuffer: *mut core::ffi::c_void,
    pRefreshObjects: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pRefreshObjects;
    crate::stub::Call::unsupported("vkCmdRefreshObjectsKHR", "extension not advertised");
}

pub extern "C" fn vkCmdSubpassShadingHUAWEI(commandBuffer: *mut core::ffi::c_void) {
    let _ = commandBuffer;
    crate::stub::Call::unsupported("vkCmdSubpassShadingHUAWEI", "extension not advertised");
}
