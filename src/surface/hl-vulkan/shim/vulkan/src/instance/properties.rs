use super::*;

const VK_DRIVER_ID_UNKNOWN: i32 = 0;

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties2(
    physical_device: *mut c_void,
    p_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_properties as *mut VkPhysicalDeviceProperties2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceProperties(
        physical_device,
        &mut out.properties as *mut _ as *mut c_void,
    );
    let mut node = out.p_next as *mut VkBaseOutStructure;
    while let Some(n) = unsafe { node.as_mut() } {
        if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES {
            if let Some(d) = unsafe { (node as *mut VkPhysicalDeviceDriverProperties).as_mut() } {
                // No Khronos driver ID is assigned to hl. UNKNOWN is truthful; borrowing another
                // implementation's ID can make clients classify this Metal-backed device as software.
                d.driver_id = VK_DRIVER_ID_UNKNOWN;
                Name::write(&mut d.driver_name, "hl");
                Name::write(&mut d.driver_info, "hl Metal (Vulkan) 0.1");
                // No Vulkan CTS conformance version has been assigned to this driver.
                d.conformance_version = VkConformanceVersion {
                    major: 0,
                    minor: 0,
                    subminor: 0,
                    patch: 0,
                };
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES {
            if let Some(m) =
                unsafe { (node as *mut VkPhysicalDeviceMaintenance3Properties).as_mut() }
            {
                m.max_per_set_descriptors = 1_000_000;
                m.max_memory_allocation_size = 1 << 31; // 2 GiB
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES {
            if let Some(m) =
                unsafe { (node as *mut VkPhysicalDeviceMaintenance4Properties).as_mut() }
            {
                // A truthful upper bound: the executor residency budget backs buffers up to the same
                // 2 GiB ceiling as maintenance3's maxMemoryAllocationSize. wgpu-hal reads maxBufferSize
                // from here (maintenance4 is core 1.3); a zero would fail device creation before submit.
                m.max_buffer_size = 1 << 31; // 2 GiB
            }
        }
        node = n.p_next;
    }
}

/// `vkGetPhysicalDeviceFeatures2` — the base 1.0 feature set, plus the promoted-feature pNext structs we
/// really back. `VkPhysicalDeviceDynamicRenderingFeatures::dynamicRendering` is set `VK_TRUE` (backed by
/// the `cmd_begin_rendering` lowering + the advertised `VK_KHR_dynamic_rendering` device extension). Every
/// OTHER promoted-feature struct is left as the app zero-initialized it (`VK_FALSE`) — we advertise no
/// feature we do not implement. The chain is preserved.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures2(
    physical_device: *mut c_void,
    p_features: *mut c_void,
) {
    let Some(out) = (unsafe { (p_features as *mut VkPhysicalDeviceFeatures2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceFeatures(physical_device, &mut out.features as *mut _ as *mut c_void);
    let mut node = out.p_next as *mut VkBaseOutStructure;
    while let Some(n) = unsafe { node.as_mut() } {
        if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES {
            if let Some(f) =
                unsafe { (node as *mut VkPhysicalDeviceDynamicRenderingFeatures).as_mut() }
            {
                f.dynamic_rendering = VK_TRUE;
            }
        }
        node = n.p_next;
    }
}

/// `vkGetPhysicalDeviceMemoryProperties2` — delegates to the 1.0 memory-properties fill.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties2(
    physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    let Some(out) =
        (unsafe { (p_memory_properties as *mut VkPhysicalDeviceMemoryProperties2).as_mut() })
    else {
        return;
    };
    vkGetPhysicalDeviceMemoryProperties(
        physical_device,
        &mut out.memory_properties as *mut _ as *mut c_void,
    );
}

/// `vkGetPhysicalDeviceQueueFamilyProperties2` — delegates to the 1.0 queue-family fill.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties2(
    physical_device: *mut c_void,
    p_queue_family_property_count: *mut u32,
    p_queue_family_properties: *mut c_void,
) {
    if p_queue_family_property_count.is_null() {
        return;
    }
    if p_queue_family_properties.is_null() {
        vkGetPhysicalDeviceQueueFamilyProperties(
            physical_device,
            p_queue_family_property_count,
            core::ptr::null_mut(),
        );
        return;
    }
    if unsafe { *p_queue_family_property_count } < 1 {
        unsafe { *p_queue_family_property_count = 0 };
        return;
    }
    let out = p_queue_family_properties as *mut VkQueueFamilyProperties2;
    if let Some(o) = unsafe { out.as_mut() } {
        vkGetPhysicalDeviceQueueFamilyProperties(
            physical_device,
            p_queue_family_property_count,
            &mut o.queue_family_properties as *mut _ as *mut c_void,
        );
    }
    unsafe { *p_queue_family_property_count = 1 };
}

/// `vkGetPhysicalDeviceFormatProperties2` — delegates to the 1.0 per-format feature fill.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties2(
    physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_format_properties as *mut VkFormatProperties2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceFormatProperties(
        physical_device,
        format,
        &mut out.format_properties as *mut _ as *mut c_void,
    );
}

// ==================================================================================================
// the `...2KHR` aliases (VK_KHR_get_physical_device_properties2) — delegate to the promoted-core bodies
// ==================================================================================================
// Each is the pre-promotion `KHR` name of the identical core-1.1 query; it forwards verbatim so an app /
// loader that resolves the `KHR` spelling gets the same truthful answer as the core entry point.

/// `vkGetPhysicalDeviceProperties2KHR` — alias of the core [`vkGetPhysicalDeviceProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties2KHR(
    physical_device: *mut c_void,
    p_properties: *mut c_void,
) {
    vkGetPhysicalDeviceProperties2(physical_device, p_properties)
}

/// `vkGetPhysicalDeviceFeatures2KHR` — alias of the core [`vkGetPhysicalDeviceFeatures2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures2KHR(
    physical_device: *mut c_void,
    p_features: *mut c_void,
) {
    vkGetPhysicalDeviceFeatures2(physical_device, p_features)
}

/// `vkGetPhysicalDeviceMemoryProperties2KHR` — alias of the core [`vkGetPhysicalDeviceMemoryProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties2KHR(
    physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    vkGetPhysicalDeviceMemoryProperties2(physical_device, p_memory_properties)
}

/// `vkGetPhysicalDeviceQueueFamilyProperties2KHR` — alias of the core
/// [`vkGetPhysicalDeviceQueueFamilyProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties2KHR(
    physical_device: *mut c_void,
    p_queue_family_property_count: *mut u32,
    p_queue_family_properties: *mut c_void,
) {
    vkGetPhysicalDeviceQueueFamilyProperties2(
        physical_device,
        p_queue_family_property_count,
        p_queue_family_properties,
    )
}

/// `vkGetPhysicalDeviceFormatProperties2KHR` — alias of the core [`vkGetPhysicalDeviceFormatProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties2KHR(
    physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    vkGetPhysicalDeviceFormatProperties2(physical_device, format, p_format_properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_properties_do_not_claim_a_software_driver_identity() {
        let mut driver: VkPhysicalDeviceDriverProperties = unsafe { core::mem::zeroed() };
        driver.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES;
        let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut driver as *mut _ as *mut c_void;

        vkGetPhysicalDeviceProperties2(
            core::ptr::null_mut(),
            &mut properties as *mut _ as *mut c_void,
        );

        assert_eq!(driver.driver_id, VK_DRIVER_ID_UNKNOWN);
        assert_ne!(driver.driver_id, 8, "must not claim Mesa LLVMPipe");
        assert_eq!(driver.conformance_version.major, 0);
        assert_eq!(driver.conformance_version.minor, 0);
        assert_eq!(driver.conformance_version.subminor, 0);
        assert_eq!(driver.conformance_version.patch, 0);
    }
}
