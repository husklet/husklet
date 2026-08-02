use super::*;

/// `VkPhysicalDeviceVulkan13Properties`, in exact `vk.xml` order.
///
/// The 30 integer-dot-product acceleration flags are one contiguous array of `VkBool32`; naming them
/// individually would not change their C ABI and would invite transcription drift.
#[repr(C)]
pub struct VkPhysicalDeviceVulkan13Properties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub min_subgroup_size: u32,
    pub max_subgroup_size: u32,
    pub max_compute_workgroup_subgroups: u32,
    pub required_subgroup_size_stages: VkFlags,
    pub max_inline_uniform_block_size: u32,
    pub max_per_stage_descriptor_inline_uniform_blocks: u32,
    pub max_per_stage_descriptor_update_after_bind_inline_uniform_blocks: u32,
    pub max_descriptor_set_inline_uniform_blocks: u32,
    pub max_descriptor_set_update_after_bind_inline_uniform_blocks: u32,
    pub max_inline_uniform_total_size: u32,
    pub integer_dot_product_accelerated: [VkBool32; 30],
    pub storage_texel_buffer_offset_alignment_bytes: VkDeviceSize,
    pub storage_texel_buffer_offset_single_texel_alignment: VkBool32,
    pub uniform_texel_buffer_offset_alignment_bytes: VkDeviceSize,
    pub uniform_texel_buffer_offset_single_texel_alignment: VkBool32,
    pub max_buffer_size: VkDeviceSize,
}

#[repr(C)]
pub struct VkPhysicalDeviceSubgroupSizeControlProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub min_subgroup_size: u32,
    pub max_subgroup_size: u32,
    pub max_compute_workgroup_subgroups: u32,
    pub required_subgroup_size_stages: VkFlags,
}

#[repr(C)]
pub struct VkPhysicalDeviceInlineUniformBlockProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub max_inline_uniform_block_size: u32,
    pub max_per_stage_descriptor_inline_uniform_blocks: u32,
    pub max_per_stage_descriptor_update_after_bind_inline_uniform_blocks: u32,
    pub max_descriptor_set_inline_uniform_blocks: u32,
    pub max_descriptor_set_update_after_bind_inline_uniform_blocks: u32,
}

#[repr(C)]
pub struct VkPhysicalDeviceShaderIntegerDotProductProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub accelerated: [VkBool32; 30],
}

#[repr(C)]
pub struct VkPhysicalDeviceTexelBufferAlignmentProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub storage_texel_buffer_offset_alignment_bytes: VkDeviceSize,
    pub storage_texel_buffer_offset_single_texel_alignment: VkBool32,
    pub uniform_texel_buffer_offset_alignment_bytes: VkDeviceSize,
    pub uniform_texel_buffer_offset_single_texel_alignment: VkBool32,
}
