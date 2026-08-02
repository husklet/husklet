use super::*;

/// `vkGetPhysicalDeviceProperties2` fills the maintenance4 pNext node with a real `maxBufferSize`
/// (2 GiB) and preserves the chain. wgpu-hal (Vulkan 1.3+) reads `max_buffer_size` from here; a
/// zero-initialized node (an unfilled branch) made it reject our device with
/// "Limit 'max_buffer_size' value … is better than allowed 0" and fall back to llvmpipe.
#[test]
fn properties2_fills_maintenance4_max_buffer_size_and_preserves_chain() {
    // A maintenance4 node the app zero-inits and chains after a sentinel tail node.
    let mut tail = VkBaseOutStructure {
        s_type: 0x7FFF_0001,
        p_next: core::ptr::null_mut(),
    };
    let mut m4 = VkPhysicalDeviceMaintenance4Properties {
        s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES,
        p_next: &mut tail as *mut _ as *mut c_void,
        max_buffer_size: 0,
    };
    // SAFETY: base VkPhysicalDeviceProperties is written by the entry point; zero-init is a valid
    // starting state for the C ABI struct the loader would hand us.
    let mut props2: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
    props2.p_next = &mut m4 as *mut _ as *mut c_void;

    vkGetPhysicalDeviceProperties2(core::ptr::null_mut(), &mut props2 as *mut _ as *mut c_void);

    assert_eq!(
        m4.max_buffer_size,
        1 << 31,
        "maintenance4 maxBufferSize must be 2 GiB"
    );
    assert_eq!(
        m4.s_type, VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES,
        "sType intact"
    );
    // The chain past the filled node is preserved untouched.
    assert_eq!(
        m4.p_next, &mut tail as *mut _ as *mut c_void,
        "pNext chaining preserved"
    );
    assert_eq!(tail.s_type, 0x7FFF_0001, "downstream node untouched");
}

#[test]
fn vulkan13_properties_have_the_c_abi_layout() {
    assert_eq!(
        core::mem::size_of::<VkPhysicalDeviceVulkan13Properties>(),
        216
    );
    assert_eq!(
        core::mem::offset_of!(VkPhysicalDeviceVulkan13Properties, min_subgroup_size),
        16
    );
    assert_eq!(
        core::mem::offset_of!(
            VkPhysicalDeviceVulkan13Properties,
            integer_dot_product_accelerated
        ),
        56
    );
    assert_eq!(
        core::mem::offset_of!(VkPhysicalDeviceVulkan13Properties, max_buffer_size),
        208
    );
}

#[test]
fn properties2_fills_required_vulkan13_limits_and_preserves_chain() {
    let mut tail = VkBaseOutStructure {
        s_type: 0x7FFF_0002,
        p_next: core::ptr::null_mut(),
    };
    // SAFETY: zero is a valid query-output initialization for this C ABI structure.
    let mut v13: VkPhysicalDeviceVulkan13Properties = unsafe { core::mem::zeroed() };
    v13.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_PROPERTIES;
    v13.p_next = &mut tail as *mut _ as *mut c_void;
    // SAFETY: the entry point initializes the base properties before walking this valid output chain.
    let mut props2: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
    props2.p_next = &mut v13 as *mut _ as *mut c_void;

    vkGetPhysicalDeviceProperties2(core::ptr::null_mut(), &mut props2 as *mut _ as *mut c_void);

    assert_eq!(v13.min_subgroup_size, 32);
    assert_eq!(v13.max_subgroup_size, 32);
    assert_eq!(v13.max_compute_workgroup_subgroups, 32);
    assert_eq!(v13.required_subgroup_size_stages, 0);
    assert_eq!(v13.max_inline_uniform_block_size, 256);
    assert_eq!(v13.max_per_stage_descriptor_inline_uniform_blocks, 4);
    assert_eq!(
        v13.max_per_stage_descriptor_update_after_bind_inline_uniform_blocks,
        4
    );
    assert_eq!(v13.max_descriptor_set_inline_uniform_blocks, 4);
    assert_eq!(
        v13.max_descriptor_set_update_after_bind_inline_uniform_blocks,
        4
    );
    assert_eq!(v13.max_inline_uniform_total_size, 256);
    assert_eq!(v13.integer_dot_product_accelerated, [VK_FALSE; 30]);
    assert_eq!(v13.storage_texel_buffer_offset_alignment_bytes, 16);
    assert_eq!(
        v13.storage_texel_buffer_offset_single_texel_alignment,
        VK_FALSE
    );
    assert_eq!(v13.uniform_texel_buffer_offset_alignment_bytes, 16);
    assert_eq!(
        v13.uniform_texel_buffer_offset_single_texel_alignment,
        VK_FALSE
    );
    assert_eq!(v13.max_buffer_size, 1 << 31);
    assert_eq!(v13.p_next, &mut tail as *mut _ as *mut c_void);
    assert_eq!(tail.s_type, 0x7FFF_0002);
}

#[test]
fn vulkan13_aggregate_matches_promoted_property_structs() {
    // SAFETY: zero is the Vulkan query-output initialization for each C ABI structure below.
    let mut subgroup: VkPhysicalDeviceSubgroupSizeControlProperties =
        unsafe { core::mem::zeroed() };
    subgroup.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_SIZE_CONTROL_PROPERTIES;
    let mut inline: VkPhysicalDeviceInlineUniformBlockProperties = unsafe { core::mem::zeroed() };
    inline.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_INLINE_UNIFORM_BLOCK_PROPERTIES;
    inline.p_next = &mut subgroup as *mut _ as *mut c_void;
    let mut dot: VkPhysicalDeviceShaderIntegerDotProductProperties = unsafe { core::mem::zeroed() };
    dot.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_INTEGER_DOT_PRODUCT_PROPERTIES;
    dot.p_next = &mut inline as *mut _ as *mut c_void;
    let mut texel: VkPhysicalDeviceTexelBufferAlignmentProperties = unsafe { core::mem::zeroed() };
    texel.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TEXEL_BUFFER_ALIGNMENT_PROPERTIES;
    texel.p_next = &mut dot as *mut _ as *mut c_void;
    let mut maintenance = VkPhysicalDeviceMaintenance4Properties {
        s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES,
        p_next: &mut texel as *mut _ as *mut c_void,
        max_buffer_size: 0,
    };
    let mut promoted: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
    promoted.p_next = &mut maintenance as *mut _ as *mut c_void;
    vkGetPhysicalDeviceProperties2(
        core::ptr::null_mut(),
        &mut promoted as *mut _ as *mut c_void,
    );

    let mut aggregate: VkPhysicalDeviceVulkan13Properties = unsafe { core::mem::zeroed() };
    aggregate.s_type = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_PROPERTIES;
    let mut properties: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
    properties.p_next = &mut aggregate as *mut _ as *mut c_void;
    vkGetPhysicalDeviceProperties2(
        core::ptr::null_mut(),
        &mut properties as *mut _ as *mut c_void,
    );

    assert_eq!(subgroup.min_subgroup_size, aggregate.min_subgroup_size);
    assert_eq!(subgroup.max_subgroup_size, aggregate.max_subgroup_size);
    assert_eq!(
        subgroup.max_compute_workgroup_subgroups,
        aggregate.max_compute_workgroup_subgroups
    );
    assert_eq!(
        subgroup.required_subgroup_size_stages,
        aggregate.required_subgroup_size_stages
    );
    assert_eq!(
        inline.max_inline_uniform_block_size,
        aggregate.max_inline_uniform_block_size
    );
    assert_eq!(
        inline.max_per_stage_descriptor_inline_uniform_blocks,
        aggregate.max_per_stage_descriptor_inline_uniform_blocks
    );
    assert_eq!(dot.accelerated, aggregate.integer_dot_product_accelerated);
    assert_eq!(
        texel.storage_texel_buffer_offset_alignment_bytes,
        aggregate.storage_texel_buffer_offset_alignment_bytes
    );
    assert_eq!(
        texel.uniform_texel_buffer_offset_alignment_bytes,
        aggregate.uniform_texel_buffer_offset_alignment_bytes
    );
    assert_eq!(maintenance.max_buffer_size, aggregate.max_buffer_size);
}
