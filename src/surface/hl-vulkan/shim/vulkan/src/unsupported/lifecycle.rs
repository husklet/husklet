use super::*;

#[no_mangle]
pub extern "C" fn vkCreateCuFunctionNVX(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pFunction: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pFunction;
    unsafe {
        if !pFunction.is_null() {
            *(pFunction as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateCuFunctionNVX", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateCuModuleNVX(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pModule: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pModule;
    unsafe {
        if !pModule.is_null() {
            *(pModule as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateCuModuleNVX", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateCudaFunctionNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pFunction: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pFunction;
    unsafe {
        if !pFunction.is_null() {
            *(pFunction as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateCudaFunctionNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateCudaModuleNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pModule: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pModule;
    unsafe {
        if !pModule.is_null() {
            *(pModule as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateCudaModuleNV", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateDeferredOperationKHR(
    device: *mut core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pDeferredOperation: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pAllocator;
    let _ = pDeferredOperation;
    unsafe {
        if !pDeferredOperation.is_null() {
            *(pDeferredOperation as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateDeferredOperationKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateIndirectCommandsLayoutNV(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pIndirectCommandsLayout: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pIndirectCommandsLayout;
    unsafe {
        if !pIndirectCommandsLayout.is_null() {
            *(pIndirectCommandsLayout as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateIndirectCommandsLayoutNV",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateValidationCacheEXT(
    device: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pValidationCache: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pValidationCache;
    unsafe {
        if !pValidationCache.is_null() {
            *(pValidationCache as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateValidationCacheEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkDestroyCuFunctionNVX(
    device: *mut core::ffi::c_void,
    function: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = function;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyCuFunctionNVX", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkDestroyCuModuleNVX(
    device: *mut core::ffi::c_void,
    module: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = module;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyCuModuleNVX", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkDestroyCudaFunctionNV(
    device: *mut core::ffi::c_void,
    function: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = function;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyCudaFunctionNV", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkDestroyCudaModuleNV(
    device: *mut core::ffi::c_void,
    module: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = module;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyCudaModuleNV", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkDestroyDeferredOperationKHR(
    device: *mut core::ffi::c_void,
    operation: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = operation;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyDeferredOperationKHR", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkDestroyIndirectCommandsLayoutNV(
    device: *mut core::ffi::c_void,
    indirectCommandsLayout: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = indirectCommandsLayout;
    let _ = pAllocator;
    crate::stub::Call::unsupported(
        "vkDestroyIndirectCommandsLayoutNV",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkDestroyValidationCacheEXT(
    device: *mut core::ffi::c_void,
    validationCache: u64,
    pAllocator: *const core::ffi::c_void,
) {
    let _ = device;
    let _ = validationCache;
    let _ = pAllocator;
    crate::stub::Call::unsupported("vkDestroyValidationCacheEXT", "extension not advertised");
}
