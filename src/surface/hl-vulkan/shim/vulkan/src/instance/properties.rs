use super::*;

const VK_DRIVER_ID_UNKNOWN: i32 = 0;

/// `VkShaderStageFlagBits` / `VkSubgroupFeatureFlagBits` values used by the subgroup report.
const SHADER_STAGE_FRAGMENT: VkFlags = 0x0000_0010;
const SHADER_STAGE_COMPUTE: VkFlags = 0x0000_0020;
const SUBGROUP_FEATURE_BASIC: VkFlags = 0x0000_0001;

/// The one source of truth for how this driver identifies itself, so every spelling of the query — the
/// standalone `VkPhysicalDeviceDriverProperties`, the `VkPhysicalDeviceVulkan12Properties` aggregate a
/// Vulkan-1.2+ client actually reads, and `VkPhysicalDeviceIDProperties` — reports the same answer.
struct Identity;

impl Identity {
    const DRIVER_NAME: &'static str = "hl";
    const DRIVER_INFO: &'static str = "hl Metal (Vulkan) 0.1";
    /// No Vulkan CTS conformance version has been assigned to this driver; claiming one would be a lie.
    const CONFORMANCE: VkConformanceVersion = VkConformanceVersion {
        major: 0,
        minor: 0,
        subminor: 0,
        patch: 0,
    };
    /// Metal's SIMD group width on Apple GPUs. Reported non-zero because a client that computes a
    /// dispatch geometry from `subgroupSize` divides by it, and a zero is an immediate divide-by-zero.
    const SUBGROUP_SIZE: u32 = 32;

    fn write_ids(id: &mut VkPhysicalDeviceIDProperties) {
        let desc = StateStore::with(|state| state.physical_device());
        id.device_uuid = desc.device_uuid;
        id.driver_uuid = desc.driver_uuid;
        id.device_luid = [0; VK_LUID_SIZE];
        id.device_node_mask = 0;
        id.device_luid_valid = VK_FALSE; // no LUID: this is not a Windows/D3D device
    }
}

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
                Name::write(&mut d.driver_name, Identity::DRIVER_NAME);
                Name::write(&mut d.driver_info, Identity::DRIVER_INFO);
                d.conformance_version = Identity::CONFORMANCE;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES {
            if let Some(m) =
                unsafe { (node as *mut VkPhysicalDeviceMaintenance3Properties).as_mut() }
            {
                m.max_per_set_descriptors = 1_000_000;
                m.max_memory_allocation_size = 1 << 31; // 2 GiB
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES {
            if let Some(id) = unsafe { (node as *mut VkPhysicalDeviceIDProperties).as_mut() } {
                Identity::write_ids(id);
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES {
            if let Some(sub) = unsafe { (node as *mut VkPhysicalDeviceSubgroupProperties).as_mut() } {
                sub.subgroup_size = Identity::SUBGROUP_SIZE;
                sub.supported_stages = SHADER_STAGE_COMPUTE | SHADER_STAGE_FRAGMENT;
                sub.supported_operations = SUBGROUP_FEATURE_BASIC;
                sub.quad_operations_in_all_stages = VK_FALSE;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_PROPERTIES {
            // The 1.1 aggregate carries the same identity + subgroup members. A client at the
            // advertised 1.1+ reads them from HERE, not from the standalone structs above.
            if let Some(v11) =
                unsafe { (node as *mut VkPhysicalDeviceVulkan11PropertiesPrefix).as_mut() }
            {
                let desc = StateStore::with(|s| s.physical_device());
                v11.device_uuid = desc.device_uuid;
                v11.driver_uuid = desc.driver_uuid;
                v11.device_luid = [0; VK_LUID_SIZE];
                v11.device_node_mask = 0;
                v11.device_luid_valid = VK_FALSE; // no LUID: this is not a Windows/D3D device
                v11.subgroup_size = Identity::SUBGROUP_SIZE;
                v11.subgroup_supported_stages = SHADER_STAGE_COMPUTE | SHADER_STAGE_FRAGMENT;
                v11.subgroup_supported_operations = SUBGROUP_FEATURE_BASIC;
                v11.subgroup_quad_operations_in_all_stages = VK_FALSE;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES {
            // The 1.2 aggregate carries the driver identity. This — not
            // `VkPhysicalDeviceDriverProperties` — is where a client at the advertised 1.2+ reads the
            // driver name, which is why leaving it untouched reported a blank driver.
            if let Some(v12) =
                unsafe { (node as *mut VkPhysicalDeviceVulkan12PropertiesPrefix).as_mut() }
            {
                v12.driver_id = VK_DRIVER_ID_UNKNOWN;
                Name::write(&mut v12.driver_name, Identity::DRIVER_NAME);
                Name::write(&mut v12.driver_info, Identity::DRIVER_INFO);
                v12.conformance_version = Identity::CONFORMANCE;
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
        } else if let Some(aggregate) =
            crate::promoted_features::PromotedFeatures::matching(n.s_type)
        {
            // `VkPhysicalDeviceVulkan1{1,2,3,4}Features`: report the same answer the single-feature
            // spelling gives. Leaving the aggregate untouched made the driver self-contradicting — a
            // client at Vulkan 1.3+ reads `dynamicRendering` from here, not from the standalone struct.
            unsafe { aggregate.report(node as *mut c_void) };
        }
        node = n.p_next;
    }
}

/// `vkGetPhysicalDeviceMemoryProperties2` — delegates to the 1.0 memory-properties fill.
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
pub extern "C" fn vkGetPhysicalDeviceProperties2KHR(
    physical_device: *mut c_void,
    p_properties: *mut c_void,
) {
    vkGetPhysicalDeviceProperties2(physical_device, p_properties)
}

/// `vkGetPhysicalDeviceFeatures2KHR` — alias of the core [`vkGetPhysicalDeviceFeatures2`].
pub extern "C" fn vkGetPhysicalDeviceFeatures2KHR(
    physical_device: *mut c_void,
    p_features: *mut c_void,
) {
    vkGetPhysicalDeviceFeatures2(physical_device, p_features)
}

/// `vkGetPhysicalDeviceMemoryProperties2KHR` — alias of the core [`vkGetPhysicalDeviceMemoryProperties2`].
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties2KHR(
    physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    vkGetPhysicalDeviceMemoryProperties2(physical_device, p_memory_properties)
}

/// `vkGetPhysicalDeviceQueueFamilyProperties2KHR` — alias of the core
/// [`vkGetPhysicalDeviceQueueFamilyProperties2`].
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

    /// A Vulkan-1.2+ client reads the driver identity from the `VkPhysicalDeviceVulkan12Properties`
    /// aggregate, not from `VkPhysicalDeviceDriverProperties`. Leaving it untouched reported a blank
    /// driver name, so tools could not identify the driver at all.
    #[test]
    fn vulkan_1_2_aggregate_reports_the_driver_identity() {
        let mut v12: VkPhysicalDeviceVulkan12PropertiesPrefix = unsafe { core::mem::zeroed() };
        v12.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES;
        let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut v12 as *mut _ as *mut c_void;

        vkGetPhysicalDeviceProperties2(
            core::ptr::null_mut(),
            &mut properties as *mut _ as *mut c_void,
        );

        let name: Vec<u8> = v12
            .driver_name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        assert_eq!(String::from_utf8_lossy(&name), "hl");
        assert_eq!(v12.driver_id, VK_DRIVER_ID_UNKNOWN);
    }

    /// `subgroupSize` of 0 is not a conservative answer, it is an arithmetic trap: a client sizing a
    /// dispatch from it divides by zero.
    #[test]
    fn subgroup_size_is_reported_non_zero() {
        let mut subgroup: VkPhysicalDeviceSubgroupProperties = unsafe { core::mem::zeroed() };
        subgroup.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES;
        let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut subgroup as *mut _ as *mut c_void;

        vkGetPhysicalDeviceProperties2(
            core::ptr::null_mut(),
            &mut properties as *mut _ as *mut c_void,
        );

        assert_ne!(subgroup.subgroup_size, 0);
        assert_eq!(subgroup.subgroup_size, Identity::SUBGROUP_SIZE);
    }

    /// Device and driver UUIDs must be stable and non-zero, and must differ from each other — an
    /// all-zero UUID makes the device indistinguishable from any other driver reporting zeros.
    #[test]
    fn identity_uuids_are_non_zero_and_distinct() {
        let mut id: VkPhysicalDeviceIDProperties = unsafe { core::mem::zeroed() };
        id.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES;
        let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut id as *mut _ as *mut c_void;

        vkGetPhysicalDeviceProperties2(
            core::ptr::null_mut(),
            &mut properties as *mut _ as *mut c_void,
        );

        assert_ne!(id.device_uuid, [0u8; 16]);
        assert_ne!(id.driver_uuid, [0u8; 16]);
        assert_ne!(id.device_uuid, id.driver_uuid);
        assert_eq!(id.device_luid_valid, VK_FALSE);
    }

    /// The prefix structs must have the same offsets as the full `vk.xml` declarations, or a write
    /// through them lands on the wrong member.
    #[test]
    fn identity_prefix_layouts_match_the_registry() {
        // sType(4) + pad(4) + pNext(8), then driverID, then the two 256-byte name buffers.
        let v12: VkPhysicalDeviceVulkan12PropertiesPrefix = unsafe { core::mem::zeroed() };
        let base = &v12 as *const _ as usize;
        assert_eq!(v12.driver_name.as_ptr() as usize - base, 20);
        assert_eq!(v12.driver_info.as_ptr() as usize - base, 20 + 256);
        let v11: VkPhysicalDeviceVulkan11PropertiesPrefix = unsafe { core::mem::zeroed() };
        let base = &v11 as *const _ as usize;
        assert_eq!(v11.device_uuid.as_ptr() as usize - base, 16);
        assert_eq!(v11.driver_uuid.as_ptr() as usize - base, 32);
        assert_eq!(v11.device_luid.as_ptr() as usize - base, 48);
        let id: VkPhysicalDeviceIDProperties = unsafe { core::mem::zeroed() };
        let base = &id as *const _ as usize;
        assert_eq!(id.device_uuid.as_ptr() as usize - base, 16);
        assert_eq!(id.driver_uuid.as_ptr() as usize - base, 32);
    }
}
