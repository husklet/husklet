use super::*;

pub extern "C" fn vkCreateBufferCollectionFUCHSIA(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pCollection: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pCollection;
    unsafe {
        if !pCollection.is_null() {
            *(pCollection as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateBufferCollectionFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkCreateSemaphoreSciSyncPoolNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSemaphorePool: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSemaphorePool;
    unsafe {
        if !pSemaphorePool.is_null() {
            *(pSemaphorePool as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateSemaphoreSciSyncPoolNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkDestroyBufferCollectionFUCHSIA(
    device: *mut core::ffi::c_void,
    collection: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = collection;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroyBufferCollectionFUCHSIA",
        "extension not advertised",
    );
}

pub extern "C" fn vkDestroySemaphoreSciSyncPoolNV(
    device: *mut core::ffi::c_void,
    semaphorePool: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = semaphorePool;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroySemaphoreSciSyncPoolNV",
        "extension not advertised",
    );
}

pub extern "C" fn vkExportMetalObjectsEXT(
    device: *mut core::ffi::c_void,
    pMetalObjectsInfo: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pMetalObjectsInfo;
    crate::stub::Call::unsupported("vkExportMetalObjectsEXT", "extension not advertised");
}

pub extern "C" fn vkGetAndroidHardwareBufferPropertiesANDROID(
    device: *mut core::ffi::c_void,
    buffer: *const core::ffi::c_void,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = buffer;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetAndroidHardwareBufferPropertiesANDROID",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetBufferCollectionPropertiesFUCHSIA(
    device: *mut core::ffi::c_void,
    collection: u64,
    pProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = collection;
    let _ = pProperties;
    crate::stub::Call::unsupported(
        "vkGetBufferCollectionPropertiesFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetFenceFdKHR(
    device: *mut core::ffi::c_void,
    pGetFdInfo: *const core::ffi::c_void,
    pFd: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetFdInfo;
    let _ = pFd;
    crate::stub::Call::unsupported("vkGetFenceFdKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetFenceSciSyncFenceNV(
    device: *mut core::ffi::c_void,
    pGetSciSyncHandleInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetSciSyncHandleInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetFenceSciSyncFenceNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetFenceSciSyncObjNV(
    device: *mut core::ffi::c_void,
    pGetSciSyncHandleInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetSciSyncHandleInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetFenceSciSyncObjNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetFenceWin32HandleKHR(
    device: *mut core::ffi::c_void,
    pGetWin32HandleInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetWin32HandleInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetFenceWin32HandleKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemoryAndroidHardwareBufferANDROID(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pBuffer: *mut *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    let _ = pBuffer;
    crate::stub::Call::unsupported(
        "vkGetMemoryAndroidHardwareBufferANDROID",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemorySciBufNV(
    device: *mut core::ffi::c_void,
    pGetSciBufInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetSciBufInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetMemorySciBufNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemoryWin32HandleKHR(
    device: *mut core::ffi::c_void,
    pGetWin32HandleInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetWin32HandleInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetMemoryWin32HandleKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemoryWin32HandleNV(
    device: *mut core::ffi::c_void,
    memory: u64,
    handleType: u32,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = memory;
    let _ = handleType;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetMemoryWin32HandleNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemoryZirconHandleFUCHSIA(
    device: *mut core::ffi::c_void,
    pGetZirconHandleInfo: *const core::ffi::c_void,
    pZirconHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetZirconHandleInfo;
    let _ = pZirconHandle;
    crate::stub::Call::unsupported("vkGetMemoryZirconHandleFUCHSIA", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetMemoryZirconHandlePropertiesFUCHSIA(
    device: *mut core::ffi::c_void,
    handleType: i32,
    zirconHandle: u32,
    pMemoryZirconHandleProperties: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = handleType;
    let _ = zirconHandle;
    let _ = pMemoryZirconHandleProperties;
    crate::stub::Call::unsupported(
        "vkGetMemoryZirconHandlePropertiesFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetSemaphoreSciSyncObjNV(
    device: *mut core::ffi::c_void,
    pGetSciSyncInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetSciSyncInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetSemaphoreSciSyncObjNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetSemaphoreWin32HandleKHR(
    device: *mut core::ffi::c_void,
    pGetWin32HandleInfo: *const core::ffi::c_void,
    pHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetWin32HandleInfo;
    let _ = pHandle;
    crate::stub::Call::unsupported("vkGetSemaphoreWin32HandleKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkGetSemaphoreZirconHandleFUCHSIA(
    device: *mut core::ffi::c_void,
    pGetZirconHandleInfo: *const core::ffi::c_void,
    pZirconHandle: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pGetZirconHandleInfo;
    let _ = pZirconHandle;
    crate::stub::Call::unsupported(
        "vkGetSemaphoreZirconHandleFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportFenceFdKHR(
    device: *mut core::ffi::c_void,
    pImportFenceFdInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportFenceFdInfo;
    crate::stub::Call::unsupported("vkImportFenceFdKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportFenceSciSyncFenceNV(
    device: *mut core::ffi::c_void,
    pImportFenceSciSyncInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportFenceSciSyncInfo;
    crate::stub::Call::unsupported("vkImportFenceSciSyncFenceNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportFenceSciSyncObjNV(
    device: *mut core::ffi::c_void,
    pImportFenceSciSyncInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportFenceSciSyncInfo;
    crate::stub::Call::unsupported("vkImportFenceSciSyncObjNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportFenceWin32HandleKHR(
    device: *mut core::ffi::c_void,
    pImportFenceWin32HandleInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportFenceWin32HandleInfo;
    crate::stub::Call::unsupported("vkImportFenceWin32HandleKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportSemaphoreSciSyncObjNV(
    device: *mut core::ffi::c_void,
    pImportSemaphoreSciSyncInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportSemaphoreSciSyncInfo;
    crate::stub::Call::unsupported("vkImportSemaphoreSciSyncObjNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportSemaphoreWin32HandleKHR(
    device: *mut core::ffi::c_void,
    pImportSemaphoreWin32HandleInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportSemaphoreWin32HandleInfo;
    crate::stub::Call::unsupported(
        "vkImportSemaphoreWin32HandleKHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkImportSemaphoreZirconHandleFUCHSIA(
    device: *mut core::ffi::c_void,
    pImportSemaphoreZirconHandleInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pImportSemaphoreZirconHandleInfo;
    crate::stub::Call::unsupported(
        "vkImportSemaphoreZirconHandleFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkSetBufferCollectionBufferConstraintsFUCHSIA(
    device: *mut core::ffi::c_void,
    collection: u64,
    pBufferConstraintsInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = collection;
    let _ = pBufferConstraintsInfo;
    crate::stub::Call::unsupported(
        "vkSetBufferCollectionBufferConstraintsFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

pub extern "C" fn vkSetBufferCollectionImageConstraintsFUCHSIA(
    device: *mut core::ffi::c_void,
    collection: u64,
    pImageConstraintsInfo: *const core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = collection;
    let _ = pImageConstraintsInfo;
    crate::stub::Call::unsupported(
        "vkSetBufferCollectionImageConstraintsFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
