use super::*;

#[repr(C)]
pub struct VkApplicationInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub p_application_name: *const c_char,
    pub application_version: u32,
    pub p_engine_name: *const c_char,
    pub engine_version: u32,
    pub api_version: u32,
}

#[repr(C)]
pub struct VkInstanceCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub p_application_info: *const VkApplicationInfo,
    pub enabled_layer_count: u32,
    pub pp_enabled_layer_names: *const *const c_char,
    pub enabled_extension_count: u32,
    pub pp_enabled_extension_names: *const *const c_char,
}

#[repr(C)]
pub struct VkDeviceCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub queue_create_info_count: u32,
    pub p_queue_create_infos: *const c_void,
    pub enabled_layer_count: u32,
    pub pp_enabled_layer_names: *const *const c_char,
    pub enabled_extension_count: u32,
    pub pp_enabled_extension_names: *const *const c_char,
    pub p_enabled_features: *const VkPhysicalDeviceFeatures,
}

#[repr(C)]
pub struct VkBufferCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub size: VkDeviceSize,
    pub usage: VkFlags,
    pub sharing_mode: i32,
    pub queue_family_index_count: u32,
    pub p_queue_family_indices: *const u32,
}

#[repr(C)]
pub struct VkMemoryAllocateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub allocation_size: VkDeviceSize,
    pub memory_type_index: u32,
}

#[repr(C)]
pub struct VkShaderModuleCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub code_size: usize,
    pub p_code: *const u32,
}

#[repr(C)]
pub struct VkDescriptorSetLayoutBinding {
    pub binding: u32,
    pub descriptor_type: i32,
    pub descriptor_count: u32,
    pub stage_flags: VkFlags,
    pub p_immutable_samplers: *const u64,
}

#[repr(C)]
pub struct VkDescriptorSetLayoutCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub binding_count: u32,
    pub p_bindings: *const VkDescriptorSetLayoutBinding,
}

#[repr(C)]
pub struct VkDescriptorPoolCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub max_sets: u32,
    pub pool_size_count: u32,
    pub p_pool_sizes: *const c_void,
}

#[repr(C)]
pub struct VkDescriptorSetAllocateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub descriptor_pool: u64,
    pub descriptor_set_count: u32,
    pub p_set_layouts: *const u64,
}

#[repr(C)]
pub struct VkDescriptorBufferInfo {
    pub buffer: u64,
    pub offset: VkDeviceSize,
    pub range: VkDeviceSize,
}

/// `VkDescriptorImageInfo` (`{ VkSampler sampler; VkImageView imageView; VkImageLayout imageLayout }`) —
/// the ABI a `SAMPLER` / `COMBINED_IMAGE_SAMPLER` / `SAMPLED_IMAGE` write carries through `pImageInfo`.
#[repr(C)]
pub struct VkDescriptorImageInfo {
    pub sampler: u64,
    pub image_view: u64,
    pub image_layout: i32,
}

#[repr(C)]
pub struct VkWriteDescriptorSet {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub dst_set: u64,
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
    pub descriptor_type: i32,
    pub p_image_info: *const c_void,
    pub p_buffer_info: *const VkDescriptorBufferInfo,
    pub p_texel_buffer_view: *const c_void,
}

#[repr(C)]
pub struct VkCopyDescriptorSet {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_set: u64,
    pub src_binding: u32,
    pub src_array_element: u32,
    pub dst_set: u64,
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
}

#[repr(C)]
pub struct VkPipelineShaderStageCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub stage: VkFlags,
    pub module: u64,
    pub p_name: *const c_char,
    pub p_specialization_info: *const c_void,
}

#[repr(C)]
pub struct VkComputePipelineCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub stage: VkPipelineShaderStageCreateInfo,
    pub layout: u64,
    pub base_pipeline_handle: u64,
    pub base_pipeline_index: i32,
}

#[repr(C)]
pub struct VkPipelineLayoutCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub set_layout_count: u32,
    pub p_set_layouts: *const u64,
    pub push_constant_range_count: u32,
    pub p_push_constant_ranges: *const c_void,
}

#[repr(C)]
pub struct VkCommandBufferAllocateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub command_pool: u64,
    pub level: i32,
    pub command_buffer_count: u32,
}

#[repr(C)]
pub struct VkFenceCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
pub struct VkSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub wait_semaphore_count: u32,
    pub p_wait_semaphores: *const u64,
    pub p_wait_dst_stage_mask: *const u32,
    pub command_buffer_count: u32,
    pub p_command_buffers: *const *mut c_void,
    pub signal_semaphore_count: u32,
    pub p_signal_semaphores: *const u64,
}

// ==================================================================================================
// graphics-path structs (images / views / samplers / render pass / graphics pipeline / WSI)
// ==================================================================================================
