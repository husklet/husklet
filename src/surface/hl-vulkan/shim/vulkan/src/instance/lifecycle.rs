use super::*;

// ==================================================================================================
// instance
// ==================================================================================================

pub extern "C" fn vkCreateInstance(
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_instance: *mut *mut c_void,
) -> VkResult {
    if p_instance.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    // The app-requested API version (default to what we advertise if the ApplicationInfo is absent).
    let app_api = unsafe {
        (p_create_info as *const VkInstanceCreateInfo)
            .as_ref()
            .and_then(|ci| ci.p_application_info.as_ref())
            .map(|ai| ai.api_version)
            .filter(|&v| v != 0)
            .unwrap_or(HL_API_VERSION)
    };
    // `VkApplicationInfo::apiVersion` is the highest version the APPLICATION is written against, not a
    // requirement on the driver: an app may legally request more than a driver implements and then gate on
    // the version each physical device reports. So clamp to what is advertised — a 1.4 app gets an instance
    // and reads 1.3 from `vkGetPhysicalDeviceProperties`. Rejecting instead (which this did) made lowering
    // the advertised version look like it would break every modern client, and it would have.
    //
    // A different VARIANT is a different API (Vulkan SC, not Vulkan), and no clamp can substitute for it.
    const VARIANT: u32 = 29;
    if app_api >> VARIANT != HL_API_VERSION >> VARIANT {
        return VK_ERROR_INCOMPATIBLE_DRIVER;
    }
    StateStore::with(|s| s.instance = Some(Instance::new(app_api.min(HL_API_VERSION))));
    let token = Dispatchable::new(());
    unsafe { *p_instance = token };
    VK_SUCCESS
}

pub extern "C" fn vkDestroyInstance(instance: *mut c_void, _p_allocator: *const c_void) {
    StateStore::with(|s| s.instance = None);
    unsafe { Dispatchable::<()>::free(instance) };
}

pub extern "C" fn vkEnumerateInstanceVersion(p_api_version: *mut u32) -> VkResult {
    if p_api_version.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    unsafe { *p_api_version = HL_API_VERSION };
    VK_SUCCESS
}

/// Instance extensions the ICD really backs (allow-list, not the whole `vk.xml`): the WSI base
/// `VK_KHR_surface` + `VK_KHR_get_physical_device_properties2` (the `...2` queries below). A real app
/// gates its init on these being enumerated, so this is the key unblock. Sourced from
/// [`capability::INSTANCE_EXTENSIONS`].
pub extern "C" fn vkEnumerateInstanceExtensionProperties(
    _p_layer_name: *const c_char,
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    let exts: Vec<VkExtensionProperties> = capability::INSTANCE_EXTENSIONS
        .iter()
        .map(VkExtensionProperties::from)
        .collect();
    unsafe {
        write_enumeration(
            &exts,
            p_property_count,
            p_properties as *mut VkExtensionProperties,
        )
    }
}

/// The ICD exposes no layers (layers are discovered from layer manifests, never the driver).
pub extern "C" fn vkEnumerateInstanceLayerProperties(
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    unsafe {
        write_enumeration::<VkLayerProperties>(
            &[],
            p_property_count,
            p_properties as *mut VkLayerProperties,
        )
    }
}

// ==================================================================================================
// physical device
// ==================================================================================================
