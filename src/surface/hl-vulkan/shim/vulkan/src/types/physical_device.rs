use super::*;

mod vulkan13;
pub use vulkan13::*;

// physical-device property structs (written back to the app; layout from vk.xml)
// ==================================================================================================

pub const VK_MAX_PHYSICAL_DEVICE_NAME_SIZE: usize = 256;
pub const VK_UUID_SIZE: usize = 16;
pub const VK_MAX_MEMORY_TYPES: usize = 32;
pub const VK_MAX_MEMORY_HEAPS: usize = 16;

/// `VkPhysicalDeviceLimits` — 106 fields in exact vk.xml order (ported from `hl-shim-vk/src/state.rs`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkPhysicalDeviceLimits {
    pub max_image_dimension_1d: u32,
    pub max_image_dimension_2d: u32,
    pub max_image_dimension_3d: u32,
    pub max_image_dimension_cube: u32,
    pub max_image_array_layers: u32,
    pub max_texel_buffer_elements: u32,
    pub max_uniform_buffer_range: u32,
    pub max_storage_buffer_range: u32,
    pub max_push_constants_size: u32,
    pub max_memory_allocation_count: u32,
    pub max_sampler_allocation_count: u32,
    pub buffer_image_granularity: VkDeviceSize,
    pub sparse_address_space_size: VkDeviceSize,
    pub max_bound_descriptor_sets: u32,
    pub max_per_stage_descriptor_samplers: u32,
    pub max_per_stage_descriptor_uniform_buffers: u32,
    pub max_per_stage_descriptor_storage_buffers: u32,
    pub max_per_stage_descriptor_sampled_images: u32,
    pub max_per_stage_descriptor_storage_images: u32,
    pub max_per_stage_descriptor_input_attachments: u32,
    pub max_per_stage_resources: u32,
    pub max_descriptor_set_samplers: u32,
    pub max_descriptor_set_uniform_buffers: u32,
    pub max_descriptor_set_uniform_buffers_dynamic: u32,
    pub max_descriptor_set_storage_buffers: u32,
    pub max_descriptor_set_storage_buffers_dynamic: u32,
    pub max_descriptor_set_sampled_images: u32,
    pub max_descriptor_set_storage_images: u32,
    pub max_descriptor_set_input_attachments: u32,
    pub max_vertex_input_attributes: u32,
    pub max_vertex_input_bindings: u32,
    pub max_vertex_input_attribute_offset: u32,
    pub max_vertex_input_binding_stride: u32,
    pub max_vertex_output_components: u32,
    pub max_tessellation_generation_level: u32,
    pub max_tessellation_patch_size: u32,
    pub max_tessellation_control_per_vertex_input_components: u32,
    pub max_tessellation_control_per_vertex_output_components: u32,
    pub max_tessellation_control_per_patch_output_components: u32,
    pub max_tessellation_control_total_output_components: u32,
    pub max_tessellation_evaluation_input_components: u32,
    pub max_tessellation_evaluation_output_components: u32,
    pub max_geometry_shader_invocations: u32,
    pub max_geometry_input_components: u32,
    pub max_geometry_output_components: u32,
    pub max_geometry_output_vertices: u32,
    pub max_geometry_total_output_components: u32,
    pub max_fragment_input_components: u32,
    pub max_fragment_output_attachments: u32,
    pub max_fragment_dual_src_attachments: u32,
    pub max_fragment_combined_output_resources: u32,
    pub max_compute_shared_memory_size: u32,
    pub max_compute_work_group_count: [u32; 3],
    pub max_compute_work_group_invocations: u32,
    pub max_compute_work_group_size: [u32; 3],
    pub sub_pixel_precision_bits: u32,
    pub sub_texel_precision_bits: u32,
    pub mipmap_precision_bits: u32,
    pub max_draw_indexed_index_value: u32,
    pub max_draw_indirect_count: u32,
    pub max_sampler_lod_bias: f32,
    pub max_sampler_anisotropy: f32,
    pub max_viewports: u32,
    pub max_viewport_dimensions: [u32; 2],
    pub viewport_bounds_range: [f32; 2],
    pub viewport_sub_pixel_bits: u32,
    pub min_memory_map_alignment: usize,
    pub min_texel_buffer_offset_alignment: VkDeviceSize,
    pub min_uniform_buffer_offset_alignment: VkDeviceSize,
    pub min_storage_buffer_offset_alignment: VkDeviceSize,
    pub min_texel_offset: i32,
    pub max_texel_offset: u32,
    pub min_texel_gather_offset: i32,
    pub max_texel_gather_offset: u32,
    pub min_interpolation_offset: f32,
    pub max_interpolation_offset: f32,
    pub sub_pixel_interpolation_offset_bits: u32,
    pub max_framebuffer_width: u32,
    pub max_framebuffer_height: u32,
    pub max_framebuffer_layers: u32,
    pub framebuffer_color_sample_counts: VkFlags,
    pub framebuffer_depth_sample_counts: VkFlags,
    pub framebuffer_stencil_sample_counts: VkFlags,
    pub framebuffer_no_attachments_sample_counts: VkFlags,
    pub max_color_attachments: u32,
    pub sampled_image_color_sample_counts: VkFlags,
    pub sampled_image_integer_sample_counts: VkFlags,
    pub sampled_image_depth_sample_counts: VkFlags,
    pub sampled_image_stencil_sample_counts: VkFlags,
    pub storage_image_sample_counts: VkFlags,
    pub max_sample_mask_words: u32,
    pub timestamp_compute_and_graphics: VkBool32,
    pub timestamp_period: f32,
    pub max_clip_distances: u32,
    pub max_cull_distances: u32,
    pub max_combined_clip_and_cull_distances: u32,
    pub discrete_queue_priorities: u32,
    pub point_size_range: [f32; 2],
    pub line_width_range: [f32; 2],
    pub point_size_granularity: f32,
    pub line_width_granularity: f32,
    pub strict_lines: VkBool32,
    pub standard_sample_locations: VkBool32,
    pub optimal_buffer_copy_offset_alignment: VkDeviceSize,
    pub optimal_buffer_copy_row_pitch_alignment: VkDeviceSize,
    pub non_coherent_atom_size: VkDeviceSize,
}

/// `VkPhysicalDeviceSparseProperties` — the 5-bool tail of `VkPhysicalDeviceProperties`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkPhysicalDeviceSparseProperties {
    pub residency_standard_2d_block_shape: VkBool32,
    pub residency_standard_2d_multisample_block_shape: VkBool32,
    pub residency_standard_3d_block_shape: VkBool32,
    pub residency_aligned_mip_size: VkBool32,
    pub residency_non_resident_strict: VkBool32,
}

/// `VkPhysicalDeviceProperties`.
#[repr(C)]
pub struct VkPhysicalDeviceProperties {
    pub api_version: u32,
    pub driver_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: i32,
    pub device_name: [c_char; VK_MAX_PHYSICAL_DEVICE_NAME_SIZE],
    pub pipeline_cache_uuid: [u8; VK_UUID_SIZE],
    pub limits: VkPhysicalDeviceLimits,
    pub sparse_properties: VkPhysicalDeviceSparseProperties,
}

/// `VkPhysicalDeviceFeatures` — 55 contiguous `VkBool32`s (indexed set; see `instance.rs`).
#[repr(C)]
pub struct VkPhysicalDeviceFeatures {
    pub bits: [VkBool32; 55],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkMemoryType {
    pub property_flags: VkFlags,
    pub heap_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkMemoryHeap {
    pub size: VkDeviceSize,
    pub flags: VkFlags,
}

#[repr(C)]
pub struct VkPhysicalDeviceMemoryProperties {
    pub memory_type_count: u32,
    pub memory_types: [VkMemoryType; VK_MAX_MEMORY_TYPES],
    pub memory_heap_count: u32,
    pub memory_heaps: [VkMemoryHeap; VK_MAX_MEMORY_HEAPS],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkExtent3D {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

#[repr(C)]
pub struct VkQueueFamilyProperties {
    pub queue_flags: VkFlags,
    pub queue_count: u32,
    pub timestamp_valid_bits: u32,
    pub min_image_transfer_granularity: VkExtent3D,
}

#[repr(C)]
pub struct VkMemoryRequirements {
    pub size: VkDeviceSize,
    pub alignment: VkDeviceSize,
    pub memory_type_bits: u32,
}

// ---- enumeration + format-query structs (written back to the app; layout from vk.xml) ------------

pub const VK_MAX_EXTENSION_NAME_SIZE: usize = 256;
pub const VK_MAX_DESCRIPTION_SIZE: usize = 256;
pub const VK_MAX_DRIVER_NAME_SIZE: usize = 256;
pub const VK_MAX_DRIVER_INFO_SIZE: usize = 256;

/// `VkExtensionProperties` — one row of `vkEnumerate{Instance,Device}ExtensionProperties`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkExtensionProperties {
    pub extension_name: [c_char; VK_MAX_EXTENSION_NAME_SIZE],
    pub spec_version: u32,
}

/// `VkLayerProperties` — one row of `vkEnumerate{Instance,Device}LayerProperties` (we expose none).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkLayerProperties {
    pub layer_name: [c_char; VK_MAX_EXTENSION_NAME_SIZE],
    pub spec_version: u32,
    pub implementation_version: u32,
    pub description: [c_char; VK_MAX_DESCRIPTION_SIZE],
}

/// `VkFormatProperties` — the three per-format feature masks (`VkFormatFeatureFlags`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkFormatProperties {
    pub linear_tiling_features: VkFlags,
    pub optimal_tiling_features: VkFlags,
    pub buffer_features: VkFlags,
}

/// `VkImageFormatProperties` — the creation limits for a supported `(format, type, tiling, …)` combo.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkImageFormatProperties {
    pub max_extent: VkExtent3D,
    pub max_mip_levels: u32,
    pub max_array_layers: u32,
    pub sample_counts: VkFlags,
    pub max_resource_size: VkDeviceSize,
}

// ---- the `...2` physical-device query wrappers (VK_KHR_get_physical_device_properties2) ----------
// Each is `{ sType, pNext, <payload> }`; the entry point fills only the payload, preserving the chain.

/// A pNext-chain node header (`{ sType, pNext }`) — every Vulkan extension struct begins with this.
#[repr(C)]
pub struct VkBaseOutStructure {
    pub s_type: i32,
    pub p_next: *mut VkBaseOutStructure,
}
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_VIEW_IMAGE_FORMAT_INFO_EXT:i32=1_000_170_000;
pub const VK_STRUCTURE_TYPE_FILTER_CUBIC_IMAGE_VIEW_IMAGE_FORMAT_PROPERTIES_EXT:i32=1_000_170_001;
#[repr(C)]
pub struct VkPhysicalDeviceImageViewImageFormatInfoEXT { pub s_type:i32, pub p_next:*const c_void, pub image_view_type:i32 }
#[repr(C)]
pub struct VkFilterCubicImageViewImageFormatPropertiesEXT { pub s_type:i32, pub p_next:*mut c_void, pub filter_cubic:u32, pub filter_cubic_minmax:u32 }

#[repr(C)]
pub struct VkPhysicalDeviceProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub properties: VkPhysicalDeviceProperties,
}

pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2: i32 = 1_000_059_000;

#[repr(C)]
pub struct VkPhysicalDeviceFeatures2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub features: VkPhysicalDeviceFeatures,
}

#[repr(C)]
pub struct VkPhysicalDeviceMemoryProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub memory_properties: VkPhysicalDeviceMemoryProperties,
}

#[repr(C)]
pub struct VkQueueFamilyProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub queue_family_properties: VkQueueFamilyProperties,
}

#[repr(C)]
pub struct VkFormatProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub format_properties: VkFormatProperties,
}

/// `VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_3` — the `VK_KHR_format_feature_flags2` (core 1.3) restatement of
/// `VkFormatProperties` in 64-bit flags, chained onto `VkFormatProperties2`.
pub const VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_3: i32 = 1_000_360_000;

/// `VkFormatProperties3` — same three feature sets as [`VkFormatProperties`], widened to
/// `VkFormatFeatureFlags2` (64-bit). Every `VkFormatFeatureFlagBits` value is defined to equal its
/// `VkFormatFeatureFlagBits2` counterpart, so the 32-bit masks zero-extend without translation.
#[repr(C)]
pub struct VkFormatProperties3 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub linear_tiling_features: u64,
    pub optimal_tiling_features: u64,
    pub buffer_features: u64,
}

/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES` (a pNext payload apps read back).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES: i32 = 1_000_196_000;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES`.
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES: i32 = 1_000_168_000;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES` (maintenance4, core in Vulkan 1.3).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES: i32 = 1_000_413_001;

/// `VkConformanceVersion` — the 4-byte version tuple in `VkPhysicalDeviceDriverProperties`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkConformanceVersion {
    pub major: u8,
    pub minor: u8,
    pub subminor: u8,
    pub patch: u8,
}

/// `VkPhysicalDeviceDriverProperties` — driverID/name/info + conformance (vkcube prints these).
#[repr(C)]
pub struct VkPhysicalDeviceDriverProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub driver_id: i32,
    pub driver_name: [c_char; VK_MAX_DRIVER_NAME_SIZE],
    pub driver_info: [c_char; VK_MAX_DRIVER_INFO_SIZE],
    pub conformance_version: VkConformanceVersion,
}

/// `VkPhysicalDeviceMaintenance3Properties` — descriptor-set + allocation-size ceilings a modern app
/// (wgpu-hal) reads to bound its descriptor sizing (a zero here makes it refuse to build any set).
#[repr(C)]
pub struct VkPhysicalDeviceMaintenance3Properties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub max_per_set_descriptors: u32,
    pub max_memory_allocation_size: VkDeviceSize,
}

/// `VkPhysicalDeviceMaintenance4Properties` — the `maxBufferSize` ceiling a Vulkan-1.3 app (wgpu-hal)
/// reads to size its buffers. maintenance4 is core in Vulkan 1.3, so an app that sees our advertised
/// api_version (1.2 or later) reads `maxBufferSize` from HERE; a zero-initialized node (no branch filling it)
/// makes wgpu reject the device with "Limit 'max_buffer_size' value … is better than allowed 0".
#[repr(C)]
pub struct VkPhysicalDeviceMaintenance4Properties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub max_buffer_size: VkDeviceSize,
}

// ==================================================================================================
// *CreateInfo / *AllocateInfo input structs (read across the seam; layout from vk.xml)
// ==================================================================================================

// ---- driver / device identity pNext payloads ------------------------------------------------------
// Tools and applications read the driver's identity from these. A node the driver leaves untouched
// reports whatever the caller initialized it to — in practice a blank name, driver ID 0 and zero UUIDs,
// i.e. an unidentifiable driver.

/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES` (extnumber 72, offset 4).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES: i32 = 1_000_071_004;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES` (extnumber 95, offset 0).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES: i32 = 1_000_094_000;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_PROPERTIES` (core value 50).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_PROPERTIES: i32 = 50;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES` (core value 52).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES: i32 = 52;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_PROPERTIES` (core value 54).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_PROPERTIES: i32 = 54;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_PROPERTIES: i32 = 1_000_053_002;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_POINT_CLIPPING_PROPERTIES: i32 = 1_000_117_000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SAMPLER_FILTER_MINMAX_PROPERTIES: i32 = 1_000_130_000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_PROPERTIES: i32 = 1_000_145_002;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DESCRIPTOR_INDEXING_PROPERTIES: i32 = 1_000_161_002;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FLOAT_CONTROLS_PROPERTIES: i32 = 1_000_197_000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DEPTH_STENCIL_RESOLVE_PROPERTIES: i32 = 1_000_199_000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_PROPERTIES: i32 = 1_000_207_001;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_INLINE_UNIFORM_BLOCK_PROPERTIES: i32 = 1_000_138_001;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_SIZE_CONTROL_PROPERTIES: i32 = 1_000_225_000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_INTEGER_DOT_PRODUCT_PROPERTIES: i32 =
    1_000_280_001;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TEXEL_BUFFER_ALIGNMENT_PROPERTIES: i32 = 1_000_281_001;

pub const VK_LUID_SIZE: usize = 8;

/// `VkPhysicalDeviceIDProperties` — the complete struct (7 members, `vk.xml` order).
#[repr(C)]
pub struct VkPhysicalDeviceIDProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub device_uuid: [u8; VK_UUID_SIZE],
    pub driver_uuid: [u8; VK_UUID_SIZE],
    pub device_luid: [u8; VK_LUID_SIZE],
    pub device_node_mask: u32,
    pub device_luid_valid: VkBool32,
}

/// `VkPhysicalDeviceSubgroupProperties` — the complete struct (6 members, `vk.xml` order).
#[repr(C)]
pub struct VkPhysicalDeviceSubgroupProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub subgroup_size: u32,
    pub supported_stages: VkFlags,
    pub supported_operations: VkFlags,
    pub quad_operations_in_all_stages: VkBool32,
}

/// The leading members of `VkPhysicalDeviceVulkan11Properties`, in exact `vk.xml` order.
///
/// A deliberate PREFIX, not the whole struct: only these fields are written, and a `#[repr(C)]` prefix
/// of a C struct has identical offsets to the full declaration, so writing through it can never disturb
/// the members beyond it. The tail (multiview/protected/descriptor ceilings) is left alone rather than
/// declared, because a hand-transcribed 17-member layout that drifts from `vk.xml` would write garbage
/// into the application — strictly worse than leaving zeros.
#[repr(C)]
pub struct VkPhysicalDeviceVulkan11PropertiesPrefix {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub device_uuid: [u8; VK_UUID_SIZE],
    pub driver_uuid: [u8; VK_UUID_SIZE],
    pub device_luid: [u8; VK_LUID_SIZE],
    pub device_node_mask: u32,
    pub device_luid_valid: VkBool32,
    pub subgroup_size: u32,
    pub subgroup_supported_stages: VkFlags,
    pub subgroup_supported_operations: VkFlags,
    pub subgroup_quad_operations_in_all_stages: VkBool32,
}

#[repr(C)]
pub struct VkPhysicalDeviceVulkan11Properties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub device_uuid: [u8; VK_UUID_SIZE],
    pub driver_uuid: [u8; VK_UUID_SIZE],
    pub device_luid: [u8; VK_LUID_SIZE],
    pub device_node_mask: u32,
    pub device_luid_valid: VkBool32,
    pub subgroup_size: u32,
    pub subgroup_supported_stages: VkFlags,
    pub subgroup_supported_operations: VkFlags,
    pub subgroup_quad_operations_in_all_stages: VkBool32,
    pub point_clipping_behavior: i32,
    pub max_multiview_view_count: u32,
    pub max_multiview_instance_index: u32,
    pub protected_no_fault: VkBool32,
    pub max_per_set_descriptors: u32,
    pub max_memory_allocation_size: VkDeviceSize,
}

#[repr(C)]
pub struct VkPhysicalDeviceMultiviewProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub max_multiview_view_count: u32,
    pub max_multiview_instance_index: u32,
}

#[repr(C)]
pub struct VkPhysicalDevicePointClippingProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub point_clipping_behavior: i32,
}

#[repr(C)]
pub struct VkPhysicalDeviceSamplerFilterMinmaxProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub filter_minmax_single_component_formats: VkBool32,
    pub filter_minmax_image_component_mapping: VkBool32,
}

#[repr(C)]
pub struct VkPhysicalDeviceProtectedMemoryProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub protected_no_fault: VkBool32,
}

#[repr(C)]
pub struct VkPhysicalDeviceDescriptorIndexingProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub max_update_after_bind_descriptors_in_all_pools: u32,
    pub shader_uniform_buffer_array_non_uniform_indexing_native: VkBool32,
    pub shader_sampled_image_array_non_uniform_indexing_native: VkBool32,
    pub shader_storage_buffer_array_non_uniform_indexing_native: VkBool32,
    pub shader_storage_image_array_non_uniform_indexing_native: VkBool32,
    pub shader_input_attachment_array_non_uniform_indexing_native: VkBool32,
    pub robust_buffer_access_update_after_bind: VkBool32,
    pub quad_divergent_implicit_lod: VkBool32,
    pub max_per_stage_descriptor_update_after_bind_samplers: u32,
    pub max_per_stage_descriptor_update_after_bind_uniform_buffers: u32,
    pub max_per_stage_descriptor_update_after_bind_storage_buffers: u32,
    pub max_per_stage_descriptor_update_after_bind_sampled_images: u32,
    pub max_per_stage_descriptor_update_after_bind_storage_images: u32,
    pub max_per_stage_descriptor_update_after_bind_input_attachments: u32,
    pub max_per_stage_update_after_bind_resources: u32,
    pub max_descriptor_set_update_after_bind_samplers: u32,
    pub max_descriptor_set_update_after_bind_uniform_buffers: u32,
    pub max_descriptor_set_update_after_bind_uniform_buffers_dynamic: u32,
    pub max_descriptor_set_update_after_bind_storage_buffers: u32,
    pub max_descriptor_set_update_after_bind_storage_buffers_dynamic: u32,
    pub max_descriptor_set_update_after_bind_sampled_images: u32,
    pub max_descriptor_set_update_after_bind_storage_images: u32,
    pub max_descriptor_set_update_after_bind_input_attachments: u32,
}

#[repr(C)]
pub struct VkPhysicalDeviceFloatControlsProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub denorm_behavior_independence: i32,
    pub rounding_mode_independence: i32,
    pub shader_signed_zero_inf_nan_preserve_float16: VkBool32,
    pub shader_signed_zero_inf_nan_preserve_float32: VkBool32,
    pub shader_signed_zero_inf_nan_preserve_float64: VkBool32,
    pub shader_denorm_preserve_float16: VkBool32,
    pub shader_denorm_preserve_float32: VkBool32,
    pub shader_denorm_preserve_float64: VkBool32,
    pub shader_denorm_flush_to_zero_float16: VkBool32,
    pub shader_denorm_flush_to_zero_float32: VkBool32,
    pub shader_denorm_flush_to_zero_float64: VkBool32,
    pub shader_rounding_mode_rte_float16: VkBool32,
    pub shader_rounding_mode_rte_float32: VkBool32,
    pub shader_rounding_mode_rte_float64: VkBool32,
    pub shader_rounding_mode_rtz_float16: VkBool32,
    pub shader_rounding_mode_rtz_float32: VkBool32,
    pub shader_rounding_mode_rtz_float64: VkBool32,
}

#[repr(C)]
pub struct VkPhysicalDeviceDepthStencilResolveProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub supported_depth_resolve_modes: VkFlags,
    pub supported_stencil_resolve_modes: VkFlags,
    pub independent_resolve_none: VkBool32,
    pub independent_resolve: VkBool32,
}

#[repr(C)]
pub struct VkPhysicalDeviceTimelineSemaphoreProperties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub max_timeline_semaphore_value_difference: u64,
}

/// The leading members of `VkPhysicalDeviceVulkan12Properties`, in exact `vk.xml` order — the driver
/// identity quartet. A deliberate prefix for the same reason as
/// [`VkPhysicalDeviceVulkan11PropertiesPrefix`]. This is where a client at the advertised Vulkan 1.2+
/// reads the driver name, NOT `VkPhysicalDeviceDriverProperties`.
#[repr(C)]
pub struct VkPhysicalDeviceVulkan12PropertiesPrefix {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub driver_id: i32,
    pub driver_name: [c_char; VK_MAX_DRIVER_NAME_SIZE],
    pub driver_info: [c_char; VK_MAX_DRIVER_INFO_SIZE],
    pub conformance_version: VkConformanceVersion,
}

#[repr(C)]
pub struct VkPhysicalDeviceVulkan12Properties {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub driver_id: i32,
    pub driver_name: [c_char; VK_MAX_DRIVER_NAME_SIZE],
    pub driver_info: [c_char; VK_MAX_DRIVER_INFO_SIZE],
    pub conformance_version: VkConformanceVersion,
    pub denorm_behavior_independence: i32,
    pub rounding_mode_independence: i32,
    pub shader_signed_zero_inf_nan_preserve_float16: VkBool32,
    pub shader_signed_zero_inf_nan_preserve_float32: VkBool32,
    pub shader_signed_zero_inf_nan_preserve_float64: VkBool32,
    pub shader_denorm_preserve_float16: VkBool32,
    pub shader_denorm_preserve_float32: VkBool32,
    pub shader_denorm_preserve_float64: VkBool32,
    pub shader_denorm_flush_to_zero_float16: VkBool32,
    pub shader_denorm_flush_to_zero_float32: VkBool32,
    pub shader_denorm_flush_to_zero_float64: VkBool32,
    pub shader_rounding_mode_rte_float16: VkBool32,
    pub shader_rounding_mode_rte_float32: VkBool32,
    pub shader_rounding_mode_rte_float64: VkBool32,
    pub shader_rounding_mode_rtz_float16: VkBool32,
    pub shader_rounding_mode_rtz_float32: VkBool32,
    pub shader_rounding_mode_rtz_float64: VkBool32,
    pub max_update_after_bind_descriptors_in_all_pools: u32,
    pub shader_uniform_buffer_array_non_uniform_indexing_native: VkBool32,
    pub shader_sampled_image_array_non_uniform_indexing_native: VkBool32,
    pub shader_storage_buffer_array_non_uniform_indexing_native: VkBool32,
    pub shader_storage_image_array_non_uniform_indexing_native: VkBool32,
    pub shader_input_attachment_array_non_uniform_indexing_native: VkBool32,
    pub robust_buffer_access_update_after_bind: VkBool32,
    pub quad_divergent_implicit_lod: VkBool32,
    pub max_per_stage_descriptor_update_after_bind_samplers: u32,
    pub max_per_stage_descriptor_update_after_bind_uniform_buffers: u32,
    pub max_per_stage_descriptor_update_after_bind_storage_buffers: u32,
    pub max_per_stage_descriptor_update_after_bind_sampled_images: u32,
    pub max_per_stage_descriptor_update_after_bind_storage_images: u32,
    pub max_per_stage_descriptor_update_after_bind_input_attachments: u32,
    pub max_per_stage_update_after_bind_resources: u32,
    pub max_descriptor_set_update_after_bind_samplers: u32,
    pub max_descriptor_set_update_after_bind_uniform_buffers: u32,
    pub max_descriptor_set_update_after_bind_uniform_buffers_dynamic: u32,
    pub max_descriptor_set_update_after_bind_storage_buffers: u32,
    pub max_descriptor_set_update_after_bind_storage_buffers_dynamic: u32,
    pub max_descriptor_set_update_after_bind_sampled_images: u32,
    pub max_descriptor_set_update_after_bind_storage_images: u32,
    pub max_descriptor_set_update_after_bind_input_attachments: u32,
    pub supported_depth_resolve_modes: VkFlags,
    pub supported_stencil_resolve_modes: VkFlags,
    pub independent_resolve_none: VkBool32,
    pub independent_resolve: VkBool32,
    pub filter_minmax_single_component_formats: VkBool32,
    pub filter_minmax_image_component_mapping: VkBool32,
    pub max_timeline_semaphore_value_difference: u64,
    pub framebuffer_integer_color_sample_counts: VkFlags,
}
