use super::*;

mod vulkan13;

const VK_DRIVER_ID_UNKNOWN: i32 = 0;

/// `VkShaderStageFlagBits` / `VkSubgroupFeatureFlagBits` values used by the subgroup report.
const SHADER_STAGE_FRAGMENT: VkFlags = 0x0000_0010;
const SHADER_STAGE_COMPUTE: VkFlags = 0x0000_0020;
const SUBGROUP_FEATURE_BASIC: VkFlags = 0x0000_0001;
const MAX_MULTIVIEW_VIEW_COUNT: u32 = 6;
const MAX_MULTIVIEW_INSTANCE_INDEX: u32 = (1 << 27) - 1;
const MAX_PER_SET_DESCRIPTORS: u32 = 1_000_000;
const PROTECTED_NO_FAULT: VkBool32 = VK_FALSE;
const MAX_TIMELINE_SEMAPHORE_VALUE_DIFFERENCE: u64 = u64::MAX;

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
                m.max_per_set_descriptors = MAX_PER_SET_DESCRIPTORS;
                // Read from the same constant `create_buffer` refuses against, so the advertised
                // ceiling and the enforced one cannot drift apart again.
                m.max_memory_allocation_size =
                    StateStore::with(|s| s.physical_device().limits.max_buffer_size);
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES {
            if let Some(id) = unsafe { (node as *mut VkPhysicalDeviceIDProperties).as_mut() } {
                Identity::write_ids(id);
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES {
            if let Some(sub) = unsafe { (node as *mut VkPhysicalDeviceSubgroupProperties).as_mut() }
            {
                sub.subgroup_size = Identity::SUBGROUP_SIZE;
                sub.supported_stages = SHADER_STAGE_COMPUTE | SHADER_STAGE_FRAGMENT;
                sub.supported_operations = SUBGROUP_FEATURE_BASIC;
                sub.quad_operations_in_all_stages = VK_FALSE;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_PROPERTIES {
            if let Some(multiview) =
                unsafe { (node as *mut VkPhysicalDeviceMultiviewProperties).as_mut() }
            {
                multiview.max_multiview_view_count = MAX_MULTIVIEW_VIEW_COUNT;
                multiview.max_multiview_instance_index = MAX_MULTIVIEW_INSTANCE_INDEX;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_PROPERTIES {
            if let Some(protected) =
                unsafe { (node as *mut VkPhysicalDeviceProtectedMemoryProperties).as_mut() }
            {
                protected.protected_no_fault = PROTECTED_NO_FAULT;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_PROPERTIES {
            if let Some(timeline) =
                unsafe { (node as *mut VkPhysicalDeviceTimelineSemaphoreProperties).as_mut() }
            {
                timeline.max_timeline_semaphore_value_difference =
                    MAX_TIMELINE_SEMAPHORE_VALUE_DIFFERENCE;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_PROPERTIES {
            // The 1.1 aggregate carries the same identity + subgroup members. A client at the
            // advertised 1.1+ reads them from HERE, not from the standalone structs above.
            if let Some(v11) = unsafe { (node as *mut VkPhysicalDeviceVulkan11Properties).as_mut() } {
                let s_type = v11.s_type;
                let p_next = v11.p_next;
                *v11 = unsafe { core::mem::zeroed() };
                v11.s_type = s_type;
                v11.p_next = p_next;
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
                v11.max_multiview_view_count = MAX_MULTIVIEW_VIEW_COUNT;
                v11.max_multiview_instance_index = MAX_MULTIVIEW_INSTANCE_INDEX;
                v11.protected_no_fault = PROTECTED_NO_FAULT;
                v11.max_per_set_descriptors = MAX_PER_SET_DESCRIPTORS;
                v11.max_memory_allocation_size = desc.limits.max_buffer_size;
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES {
            // The 1.2 aggregate carries the driver identity. This — not
            // `VkPhysicalDeviceDriverProperties` — is where a client at the advertised 1.2+ reads the
            // driver name, which is why leaving it untouched reported a blank driver.
            if let Some(v12) = unsafe { (node as *mut VkPhysicalDeviceVulkan12Properties).as_mut() } {
                let s_type = v12.s_type;
                let p_next = v12.p_next;
                *v12 = unsafe { core::mem::zeroed() };
                v12.s_type = s_type;
                v12.p_next = p_next;
                v12.driver_id = VK_DRIVER_ID_UNKNOWN;
                Name::write(&mut v12.driver_name, Identity::DRIVER_NAME);
                Name::write(&mut v12.driver_info, Identity::DRIVER_INFO);
                v12.conformance_version = Identity::CONFORMANCE;
                v12.max_timeline_semaphore_value_difference =
                    MAX_TIMELINE_SEMAPHORE_VALUE_DIFFERENCE;
                v12.framebuffer_integer_color_sample_counts = 1;
            }
        } else if vulkan13::try_fill(node) {
            // Filled by the core-1.3 property owner, including promoted spellings.
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES {
            if let Some(m) =
                unsafe { (node as *mut VkPhysicalDeviceMaintenance4Properties).as_mut() }
            {
                // A truthful upper bound: the executor residency budget backs buffers up to the same
                // ceiling as maintenance3's maxMemoryAllocationSize. wgpu-hal reads maxBufferSize
                // from here (maintenance4 is core 1.3); a zero would fail device creation before submit.
                // Same constant `create_buffer` refuses against — one source, so it stays honest.
                m.max_buffer_size =
                    StateStore::with(|s| s.physical_device().limits.max_buffer_size);
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
        if let Some(feature) = crate::feature_structs::FeatureStruct::matching(n.s_type) {
            // False answers must overwrite caller-poisoned payloads too.
            unsafe { feature.report(node as *mut c_void) };
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

/// `vkGetPhysicalDeviceFormatProperties2` — delegates to the 1.0 per-format feature fill, then answers a
/// `VkFormatProperties3` in the pNext chain with the same masks widened to 64 bits.
///
/// The chain was previously ignored entirely. `VkFormatProperties3` is an OUTPUT structure the caller does
/// not initialise, so leaving it untouched hands back whatever the caller's stack held: the CTS's
/// `Context::getFormatProperties` declares a plain local and reads it, which is why the reported feature
/// sets looked arbitrary per format rather than uniformly empty.
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
    let mut node = out.p_next as *mut VkBaseOutStructure;
    while let Some(n) = unsafe { node.as_mut() } {
        if n.s_type == VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_3 {
            if let Some(p3) = unsafe { (node as *mut VkFormatProperties3).as_mut() } {
                p3.linear_tiling_features = u64::from(out.format_properties.linear_tiling_features);
                p3.optimal_tiling_features =
                    u64::from(out.format_properties.optimal_tiling_features);
                p3.buffer_features = u64::from(out.format_properties.buffer_features);
            }
        }
        node = n.p_next;
    }
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

    /// A `VkFormatProperties3` in the chain must be answered, and answered with the SAME feature sets the
    /// 1.0 query reports. The struct is caller-uninitialised, so a poisoned buffer here stands in for the
    /// live stack garbage a real caller would otherwise read back as capabilities.
    #[test]
    fn format_properties3_in_the_chain_mirrors_the_ten_query() {
        let _g = crate::tests::test_guard();
        let mut p3: VkFormatProperties3 = unsafe { core::mem::zeroed() };
        p3.s_type = VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_3;
        p3.linear_tiling_features = u64::MAX;
        p3.optimal_tiling_features = u64::MAX;
        p3.buffer_features = u64::MAX;
        let mut properties: VkFormatProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut p3 as *mut _ as *mut c_void;

        const R8G8B8A8_UNORM: i32 = 37;
        vkGetPhysicalDeviceFormatProperties2(
            core::ptr::null_mut(),
            R8G8B8A8_UNORM,
            &mut properties as *mut _ as *mut c_void,
        );

        let mut ten: VkFormatProperties = unsafe { core::mem::zeroed() };
        vkGetPhysicalDeviceFormatProperties(
            core::ptr::null_mut(),
            R8G8B8A8_UNORM,
            &mut ten as *mut _ as *mut c_void,
        );
        assert_ne!(
            ten.optimal_tiling_features, 0,
            "the fixture format must have features"
        );
        assert_eq!(
            p3.linear_tiling_features,
            u64::from(ten.linear_tiling_features)
        );
        assert_eq!(
            p3.optimal_tiling_features,
            u64::from(ten.optimal_tiling_features)
        );
        assert_eq!(p3.buffer_features, u64::from(ten.buffer_features));
    }

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
        let mut v12: VkPhysicalDeviceVulkan12Properties = unsafe { core::mem::zeroed() };
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
        assert_eq!(v12.framebuffer_integer_color_sample_counts, 1);
        assert_eq!(v12.max_timeline_semaphore_value_difference, u64::MAX);
    }

    #[test]
    fn vulkan_1_1_and_multiview_properties_share_required_limits() {
        // This value is externally defined by Vulkan, so do not let an internally consistent typo make
        // this test exercise a structure type that applications never send.
        assert_eq!(VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_PROPERTIES, 1_000_053_002);
        let mut multiview: VkPhysicalDeviceMultiviewProperties = unsafe { core::mem::zeroed() };
        multiview.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_PROPERTIES;
        let mut v11: VkPhysicalDeviceVulkan11Properties = unsafe { core::mem::zeroed() };
        v11.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_PROPERTIES;
        v11.p_next = &mut multiview as *mut _ as *mut c_void;
        let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut v11 as *mut _ as *mut c_void;

        vkGetPhysicalDeviceProperties2(
            core::ptr::null_mut(),
            &mut properties as *mut _ as *mut c_void,
        );

        assert_eq!(v11.max_multiview_view_count, MAX_MULTIVIEW_VIEW_COUNT);
        assert_eq!(v11.max_multiview_instance_index, MAX_MULTIVIEW_INSTANCE_INDEX);
        assert_eq!(v11.max_multiview_view_count, multiview.max_multiview_view_count);
        assert_eq!(
            v11.max_multiview_instance_index,
            multiview.max_multiview_instance_index
        );
        assert_eq!(v11.max_per_set_descriptors, MAX_PER_SET_DESCRIPTORS);
        assert!(v11.max_memory_allocation_size >= 1 << 30);
    }

    #[test]
    fn standalone_promoted_properties_match_aggregates() {
        // These are externally assigned registry values. Literal assertions ensure an internally
        // consistent typo cannot make the query silently skip the structures applications send.
        assert_eq!(
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_PROPERTIES,
            1_000_145_002
        );
        assert_eq!(
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_PROPERTIES,
            1_000_207_001
        );

        let mut protected: VkPhysicalDeviceProtectedMemoryProperties =
            unsafe { core::mem::zeroed() };
        protected.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_PROPERTIES;
        protected.protected_no_fault = 0xa5a5_a5a5;
        let mut timeline: VkPhysicalDeviceTimelineSemaphoreProperties =
            unsafe { core::mem::zeroed() };
        timeline.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_PROPERTIES;
        timeline.max_timeline_semaphore_value_difference = 0xa5a5_a5a5_a5a5_a5a5;
        protected.p_next = &mut timeline as *mut _ as *mut c_void;

        let mut v11: VkPhysicalDeviceVulkan11Properties = unsafe { core::mem::zeroed() };
        v11.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_PROPERTIES;
        v11.p_next = &mut protected as *mut _ as *mut c_void;
        let mut v12: VkPhysicalDeviceVulkan12Properties = unsafe { core::mem::zeroed() };
        v12.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES;
        timeline.p_next = &mut v12 as *mut _ as *mut c_void;
        let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
        properties.p_next = &mut v11 as *mut _ as *mut c_void;

        vkGetPhysicalDeviceProperties2(
            core::ptr::null_mut(),
            &mut properties as *mut _ as *mut c_void,
        );

        assert_eq!(protected.protected_no_fault, v11.protected_no_fault);
        assert_eq!(protected.protected_no_fault, VK_FALSE);
        assert_eq!(
            timeline.max_timeline_semaphore_value_difference,
            v12.max_timeline_semaphore_value_difference
        );
        assert_eq!(timeline.max_timeline_semaphore_value_difference, u64::MAX);
        assert_eq!(core::mem::size_of_val(&protected), 24);
        assert_eq!(core::mem::size_of_val(&timeline), 24);
        let protected_base = &protected as *const _ as usize;
        let timeline_base = &timeline as *const _ as usize;
        assert_eq!(
            &protected.protected_no_fault as *const _ as usize - protected_base,
            16
        );
        assert_eq!(
            &timeline.max_timeline_semaphore_value_difference as *const _ as usize - timeline_base,
            16
        );
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
