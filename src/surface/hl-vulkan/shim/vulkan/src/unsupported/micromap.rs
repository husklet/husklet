use super::*;

#[no_mangle]
pub extern "C" fn vkBuildMicromapsEXT(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    infoCount: u32,
    pInfos: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = infoCount;
    let _ = pInfos;
    crate::stub::Call::unsupported("vkBuildMicromapsEXT", "extension family not modeled");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCmdBuildMicromapsEXT(
    commandBuffer: *mut core::ffi::c_void,
    infoCount: u32,
    pInfos: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = infoCount;
    let _ = pInfos;
    crate::stub::Call::unsupported("vkCmdBuildMicromapsEXT", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToMicromapEXT(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCmdCopyMemoryToMicromapEXT",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdCopyMicromapEXT(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkCmdCopyMicromapEXT", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdCopyMicromapToMemoryEXT(
    commandBuffer: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pInfo;
    crate::stub::Call::unsupported(
        "vkCmdCopyMicromapToMemoryEXT",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdWriteMicromapsPropertiesEXT(
    commandBuffer: *mut core::ffi::c_void,
    micromapCount: u32,
    pMicromaps: *const core::ffi::c_void,
    queryType: i32,
    queryPool: u64,
    firstQuery: u32,
) {
    let _ = commandBuffer;
    let _ = micromapCount;
    let _ = pMicromaps;
    let _ = queryType;
    let _ = queryPool;
    let _ = firstQuery;
    crate::stub::Call::unsupported(
        "vkCmdWriteMicromapsPropertiesEXT",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCopyMemoryToMicromapEXT(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkCopyMemoryToMicromapEXT", "extension family not modeled");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCopyMicromapEXT(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkCopyMicromapEXT", "extension family not modeled");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCopyMicromapToMemoryEXT(
    device: *mut core::ffi::c_void,
    deferredOperation: u64,
    pInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = deferredOperation;
    let _ = pInfo;
    crate::stub::Call::unsupported("vkCopyMicromapToMemoryEXT", "extension family not modeled");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateMicromapEXT(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pMicromap: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pMicromap;
    unsafe {
        if !pMicromap.is_null() {
            *(pMicromap as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateMicromapEXT", "extension family not modeled");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkDestroyMicromapEXT(
    device: *mut core::ffi::c_void,
    micromap: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = micromap;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyMicromapEXT", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkGetDeviceMicromapCompatibilityEXT(
    device: *mut core::ffi::c_void,
    pVersionInfo: *const core::ffi::c_void,
    pCompatibility: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pVersionInfo;
    let _ = pCompatibility;
    crate::stub::Call::unsupported(
        "vkGetDeviceMicromapCompatibilityEXT",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkGetMicromapBuildSizesEXT(
    device: *mut core::ffi::c_void,
    buildType: i32,
    pBuildInfo: *const core::ffi::c_void,
    pSizeInfo: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = buildType;
    let _ = pBuildInfo;
    let _ = pSizeInfo;
    crate::stub::Call::unsupported("vkGetMicromapBuildSizesEXT", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkWriteMicromapsPropertiesEXT(
    device: *mut core::ffi::c_void,
    micromapCount: u32,
    pMicromaps: *const core::ffi::c_void,
    queryType: i32,
    dataSize: usize,
    pData: *mut core::ffi::c_void,
    stride: usize,
) -> i32 {
    let _ = device;
    let _ = micromapCount;
    let _ = pMicromaps;
    let _ = queryType;
    let _ = dataSize;
    let _ = pData;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkWriteMicromapsPropertiesEXT",
        "extension family not modeled",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
