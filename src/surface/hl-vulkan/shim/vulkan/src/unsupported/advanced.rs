use super::*;

#[no_mangle]
pub extern "C" fn vkBindAccelerationStructureMemoryNV(
    device: *mut core::ffi::c_void,
    bindInfoCount: u32,
    pBindInfos: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = bindInfoCount;
    let _ = pBindInfos;
    crate::stub::Call::unsupported(
        "vkBindAccelerationStructureMemoryNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkBuildAccelerationStructuresKHR(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    infoCount: u32,
    pInfos: *const core::ffi::c_void,
    ppBuildRangeInfos: *const *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = infoCount;
    let _ = pInfos;
    let _ = ppBuildRangeInfos;
    crate::stub::Call::unsupported(
        "vkBuildAccelerationStructuresKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructureNV(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    instanceData: u64,
    instanceOffset: u64,
    update: u32,
    dst: u64,
    src: u64,
    scratch: u64,
    scratchOffset: u64,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    let _ = instanceData;
    let _ = instanceOffset;
    let _ = update;
    let _ = dst;
    let _ = src;
    let _ = scratch;
    let _ = scratchOffset;
    crate::stub::Call::unsupported(
        "vkCmdBuildAccelerationStructureNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructuresIndirectKHR(
    commandBuffer: *mut core::ffi::c_void,
    infoCount: u32,
    pInfos: *const core::ffi::c_void,
    pIndirectDeviceAddresses: *const core::ffi::c_void,
    pIndirectStrides: *const core::ffi::c_void,
    ppMaxPrimitiveCounts: *const *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = infoCount;
    let _ = pInfos;
    let _ = pIndirectDeviceAddresses;
    let _ = pIndirectStrides;
    let _ = ppMaxPrimitiveCounts;
    crate::stub::Call::unsupported(
        "vkCmdBuildAccelerationStructuresIndirectKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructuresKHR(
    commandBuffer: *mut core::ffi::c_void,
    infoCount: u32,
    pInfos: *const core::ffi::c_void,
    ppBuildRangeInfos: *const *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = infoCount;
    let _ = pInfos;
    let _ = ppBuildRangeInfos;
    crate::stub::Call::unsupported(
        "vkCmdBuildAccelerationStructuresKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureKHR(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCmdCopyAccelerationStructureKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureNV(
    commandBuffer: *mut core::ffi::c_void,
    dst: u64,
    src: u64,
    mode: i32,
) {
    let _ = commandBuffer;
    let _ = dst;
    let _ = src;
    let _ = mode;
    crate::stub::Call::unsupported(
        "vkCmdCopyAccelerationStructureNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureToMemoryKHR(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCmdCopyAccelerationStructureToMemoryKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToAccelerationStructureKHR(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCmdCopyMemoryToAccelerationStructureKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdWriteAccelerationStructuresPropertiesKHR(
    commandBuffer: *mut core::ffi::c_void,
    accelerationStructureCount: u32,
    pAccelerationStructures: *const core::ffi::c_void,
    queryType: i32,
    queryPool: u64,
    firstQuery: u32,
) {
    let _ = commandBuffer;
    let _ = accelerationStructureCount;
    let _ = pAccelerationStructures;
    let _ = queryType;
    let _ = queryPool;
    let _ = firstQuery;
    crate::stub::Call::unsupported(
        "vkCmdWriteAccelerationStructuresPropertiesKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdWriteAccelerationStructuresPropertiesNV(
    commandBuffer: *mut core::ffi::c_void,
    accelerationStructureCount: u32,
    pAccelerationStructures: *const core::ffi::c_void,
    queryType: i32,
    queryPool: u64,
    firstQuery: u32,
) {
    let _ = commandBuffer;
    let _ = accelerationStructureCount;
    let _ = pAccelerationStructures;
    let _ = queryType;
    let _ = queryPool;
    let _ = firstQuery;
    crate::stub::Call::unsupported(
        "vkCmdWriteAccelerationStructuresPropertiesNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCopyAccelerationStructureKHR(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCopyAccelerationStructureKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCopyAccelerationStructureToMemoryKHR(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCopyAccelerationStructureToMemoryKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCopyMemoryToAccelerationStructureKHR(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCopyMemoryToAccelerationStructureKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateAccelerationStructureKHR(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pAccelerationStructure: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pAccelerationStructure;
    unsafe {
        if !pAccelerationStructure.is_null() {
            *(pAccelerationStructure as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateAccelerationStructureKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateAccelerationStructureNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pAccelerationStructure: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pAccelerationStructure;
    unsafe {
        if !pAccelerationStructure.is_null() {
            *(pAccelerationStructure as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateAccelerationStructureNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkDestroyAccelerationStructureKHR(
    device: *mut core::ffi::c_void,
    accelerationStructure: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = accelerationStructure;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroyAccelerationStructureKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkDestroyAccelerationStructureNV(
    device: *mut core::ffi::c_void,
    accelerationStructure: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = accelerationStructure;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroyAccelerationStructureNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureBuildSizesKHR(
    device: *mut core::ffi::c_void,
    buildType: i32,
    pBuildInfo: *const core::ffi::c_void,
    pMaxPrimitiveCounts: *const core::ffi::c_void,
    pSizeInfo: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = buildType;
    let _ = pBuildInfo;
    let _ = pMaxPrimitiveCounts;
    let _ = pSizeInfo;
    crate::stub::Call::unsupported(
        "vkGetAccelerationStructureBuildSizesKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureDeviceAddressKHR(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) -> u64 {
    let _ = device;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkGetAccelerationStructureDeviceAddressKHR",
        "extension family not modeled",
    );
    0
}

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureHandleNV(
    device: *mut core::ffi::c_void,
    accelerationStructure: u64,
    dataSize: usize,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = accelerationStructure;
    let _ = dataSize;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetAccelerationStructureHandleNV",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureMemoryRequirementsNV(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pMemoryRequirements: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pInfo;
    let _ = pMemoryRequirements;
    crate::stub::Call::unsupported(
        "vkGetAccelerationStructureMemoryRequirementsNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureOpaqueCaptureDescriptorDataEXT(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetAccelerationStructureOpaqueCaptureDescriptorDataEXT",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetDeviceAccelerationStructureCompatibilityKHR(
    device: *mut core::ffi::c_void,
    pVersionInfo: *const core::ffi::c_void,
    pCompatibility: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pVersionInfo;
    let _ = pCompatibility;
    crate::stub::Call::unsupported(
        "vkGetDeviceAccelerationStructureCompatibilityKHR",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkWriteAccelerationStructuresPropertiesKHR(
    device: *mut core::ffi::c_void,
    accelerationStructureCount: u32,
    pAccelerationStructures: *const core::ffi::c_void,
    queryType: i32,
    dataSize: usize,
    pData: *mut core::ffi::c_void,
    stride: usize,
) -> i32 {
    let _ = device;
    let _ = accelerationStructureCount;
    let _ = pAccelerationStructures;
    let _ = queryType;
    let _ = dataSize;
    let _ = pData;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkWriteAccelerationStructuresPropertiesKHR",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
