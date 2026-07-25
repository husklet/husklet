use super::*;

#[no_mangle]
pub extern "C" fn vkCreateAndroidSurfaceKHR(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateAndroidSurfaceKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateDirectFBSurfaceEXT(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateDirectFBSurfaceEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateIOSSurfaceMVK(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateIOSSurfaceMVK", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateImagePipeSurfaceFUCHSIA(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateImagePipeSurfaceFUCHSIA",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateMacOSSurfaceMVK(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateMacOSSurfaceMVK", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateMetalSurfaceEXT(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateMetalSurfaceEXT", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateStreamDescriptorSurfaceGGP(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported(
        "vkCreateStreamDescriptorSurfaceGGP",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateViSurfaceNN(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateViSurfaceNN", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkCreateWin32SurfaceKHR(
    instance: *mut core::ffi::c_void,
    pCreateInfo: *const core::ffi::c_void,
    pAllocator: *const core::ffi::c_void,
    pSurface: *mut core::ffi::c_void,
) -> i32 {
    let _ = instance;
    let _ = pCreateInfo;
    let _ = pAllocator;
    let _ = pSurface;
    unsafe {
        if !pSurface.is_null() {
            *(pSurface as *mut u64) = 0;
        }
    }
    crate::stub::Call::unsupported("vkCreateWin32SurfaceKHR", "extension not advertised");
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilities2EXT(
    physicalDevice: *mut core::ffi::c_void,
    surface: u64,
    pSurfaceCapabilities: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = surface;
    let _ = pSurfaceCapabilities;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceSurfaceCapabilities2EXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilities2KHR(
    physicalDevice: *mut core::ffi::c_void,
    pSurfaceInfo: *const core::ffi::c_void,
    pSurfaceCapabilities: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pSurfaceInfo;
    let _ = pSurfaceCapabilities;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceSurfaceCapabilities2KHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceFormats2KHR(
    physicalDevice: *mut core::ffi::c_void,
    pSurfaceInfo: *const core::ffi::c_void,
    pSurfaceFormatCount: *mut core::ffi::c_void,
    pSurfaceFormats: *mut core::ffi::c_void,
) -> i32 {
    let _ = physicalDevice;
    let _ = pSurfaceInfo;
    let _ = pSurfaceFormatCount;
    let _ = pSurfaceFormats;
    crate::stub::Call::unsupported(
        "vkGetPhysicalDeviceSurfaceFormats2KHR",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
