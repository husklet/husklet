//! The Vulkan C-ABI aliases, `VkResult` values, loader-dispatchable-handle helper, and the by-value
//! C structs the hand-written entry points read/write across the seam.
//!
//! Dispatchable handles (`VkInstance`/`VkDevice`/`VkQueue`/`VkCommandBuffer`) are opaque pointers to a
//! loader-magic'd [`Dispatchable`] object (Vulkan-Loader `vk_icd.h` contract). Non-dispatchable handles
//! are the plain `u64`s the `hl_vulkan` object model already mints. The big by-value structs
//! (`VkPhysicalDeviceProperties`, `VkBufferCreateInfo`, …) are re-declared clean-room here as
//! `#[repr(C)]` so their layout matches a real loader/app byte-for-byte — the shim carries no `ash`
//! dependency (the thinnest-seam doctrine). Struct field order + values are ported from `vk.xml` and
//! `hl-shim-vk/src/{types.rs,state.rs}`.

#![allow(non_snake_case, non_upper_case_globals, dead_code)]

use core::ffi::{c_char, c_void};

// ---- scalar aliases ------------------------------------------------------------------------------
pub type VkResult = i32;
pub type VkBool32 = u32;
pub type VkDeviceSize = u64;
pub type VkFlags = u32;

// ---- dispatchable handles (pointer to a loader-magic'd object) -----------------------------------
pub type VkInstance = *mut c_void;
pub type VkPhysicalDevice = *mut c_void;
pub type VkDevice = *mut c_void;
pub type VkQueue = *mut c_void;
pub type VkCommandBuffer = *mut c_void;

pub const VK_TRUE: VkBool32 = 1;
pub const VK_FALSE: VkBool32 = 0;

// ---- VkResult values (stable Vulkan ABI, from vk.xml) --------------------------------------------
pub const VK_SUCCESS: VkResult = 0;
pub const VK_NOT_READY: VkResult = 1;
pub const VK_TIMEOUT: VkResult = 2;
pub const VK_INCOMPLETE: VkResult = 5;
pub const VK_ERROR_OUT_OF_HOST_MEMORY: VkResult = -1;
pub const VK_ERROR_OUT_OF_DEVICE_MEMORY: VkResult = -2;
pub const VK_ERROR_INITIALIZATION_FAILED: VkResult = -3;
pub const VK_ERROR_DEVICE_LOST: VkResult = -4;
pub const VK_ERROR_MEMORY_MAP_FAILED: VkResult = -5;
pub const VK_ERROR_FEATURE_NOT_PRESENT: VkResult = -8;
pub const VK_ERROR_INCOMPATIBLE_DRIVER: VkResult = -9;
pub const VK_ERROR_UNKNOWN: VkResult = -13;
/// `VK_ERROR_SURFACE_LOST_KHR` (`VK_KHR_surface`, stable ABI) — an unknown/destroyed surface.
pub const VK_ERROR_SURFACE_LOST_KHR: VkResult = -1_000_000_000;
/// `VK_ERROR_NATIVE_WINDOW_IN_USE_KHR` — a second surface over a window already claimed by one.
pub const VK_ERROR_NATIVE_WINDOW_IN_USE_KHR: VkResult = -1_000_000_001;

/// The Vulkan API version this ICD advertises: **Vulkan 1.4.0** (mirrors `hl_vulkan::result`).
pub const HL_API_VERSION: u32 = make_api_version(0, 1, 4, 0);
pub const HL_DRIVER_VERSION: u32 = make_api_version(0, 0, 1, 0);

/// `VK_MAKE_API_VERSION(variant, major, minor, patch)` — the stable Vulkan version packing.
pub const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
}

// ==================================================================================================
// loader-dispatchable-handle ABI (ported from hl-shim-vk/src/handle.rs)
// ==================================================================================================

/// `ICD_LOADER_MAGIC` from `vk_icd.h`. The loader checks `(loaderMagic & 0xffffffff) == this`.
pub const ICD_LOADER_MAGIC: usize = 0x01CD_C0DE;

/// A dispatchable ICD object: the loader-owned slot in field 0, then the ICD's own state `T`.
/// `#[repr(C)]` so field 0 is exactly the first pointer-sized word the loader reads/writes.
#[repr(C)]
pub struct Dispatchable<T> {
    /// Owned by the loader after creation — stamped with [`ICD_LOADER_MAGIC`], never read by us.
    pub loader_data: usize,
    pub inner: T,
}

impl<T> Dispatchable<T> {
    /// Box a new dispatchable object with the loader magic stamped, returning the raw handle the ICD
    /// returns to the loader.
    pub fn new(inner: T) -> *mut c_void {
        Box::into_raw(Box::new(Dispatchable { loader_data: ICD_LOADER_MAGIC, inner })) as *mut c_void
    }
    /// Borrow the ICD state behind a dispatchable handle the loader passed back. `None` for NULL.
    ///
    /// # Safety
    /// `h` must be a handle previously returned by [`Dispatchable::new`] for this `T`, still live.
    pub unsafe fn inner<'a>(h: *mut c_void) -> Option<&'a mut T> {
        (h as *mut Dispatchable<T>).as_mut().map(|d| &mut d.inner)
    }
    /// Reclaim and drop a dispatchable handle (the `vkDestroy*` / `vkFree*` path).
    ///
    /// # Safety
    /// Same contract as [`Dispatchable::inner`]; `h` must not be used afterward.
    pub unsafe fn free(h: *mut c_void) {
        if !h.is_null() {
            drop(Box::from_raw(h as *mut Dispatchable<T>));
        }
    }
}

// ==================================================================================================
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

#[repr(C)]
pub struct VkPhysicalDeviceProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub properties: VkPhysicalDeviceProperties,
}

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
/// api_version (1.4.0) reads `maxBufferSize` from HERE; a zero-initialized node (no branch filling it)
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

/// `VK_SHADER_STAGE_VERTEX_BIT` / `..._FRAGMENT_BIT` (from vk.xml) — classify a pipeline stage.
pub const VK_SHADER_STAGE_VERTEX_BIT: u32 = 0x0000_0001;
pub const VK_SHADER_STAGE_FRAGMENT_BIT: u32 = 0x0000_0010;
/// `VK_ATTACHMENT_LOAD_OP_CLEAR` (a render pass's first color attachment clears when its loadOp is this).
pub const VK_ATTACHMENT_LOAD_OP_CLEAR: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkExtent2D {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkOffset2D {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkRect2D {
    pub offset: VkOffset2D,
    pub extent: VkExtent2D,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkViewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

#[repr(C)]
pub struct VkImageCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub image_type: i32,
    pub format: i32,
    pub extent: VkExtent3D,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub samples: i32,
    pub tiling: i32,
    pub usage: VkFlags,
    pub sharing_mode: i32,
    pub queue_family_index_count: u32,
    pub p_queue_family_indices: *const u32,
    pub initial_layout: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkComponentMapping {
    pub r: i32,
    pub g: i32,
    pub b: i32,
    pub a: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkImageSubresourceRange {
    pub aspect_mask: VkFlags,
    pub base_mip_level: u32,
    pub level_count: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

#[repr(C)]
pub struct VkImageViewCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub image: u64,
    pub view_type: i32,
    pub format: i32,
    pub components: VkComponentMapping,
    pub subresource_range: VkImageSubresourceRange,
}

#[repr(C)]
pub struct VkSamplerCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub mag_filter: i32,
    pub min_filter: i32,
    pub mipmap_mode: i32,
    pub address_mode_u: i32,
    pub address_mode_v: i32,
    pub address_mode_w: i32,
    pub mip_lod_bias: f32,
    pub anisotropy_enable: VkBool32,
    pub max_anisotropy: f32,
    pub compare_enable: VkBool32,
    pub compare_op: i32,
    pub min_lod: f32,
    pub max_lod: f32,
    pub border_color: i32,
    pub unnormalized_coordinates: VkBool32,
}

#[repr(C)]
pub struct VkVertexInputBindingDescription {
    pub binding: u32,
    pub stride: u32,
    pub input_rate: i32,
}

#[repr(C)]
pub struct VkVertexInputAttributeDescription {
    pub location: u32,
    pub binding: u32,
    pub format: i32,
    pub offset: u32,
}

#[repr(C)]
pub struct VkPipelineVertexInputStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub vertex_binding_description_count: u32,
    pub p_vertex_binding_descriptions: *const VkVertexInputBindingDescription,
    pub vertex_attribute_description_count: u32,
    pub p_vertex_attribute_descriptions: *const VkVertexInputAttributeDescription,
}

#[repr(C)]
pub struct VkGraphicsPipelineCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub stage_count: u32,
    pub p_stages: *const VkPipelineShaderStageCreateInfo,
    pub p_vertex_input_state: *const VkPipelineVertexInputStateCreateInfo,
    pub p_input_assembly_state: *const c_void,
    pub p_tessellation_state: *const c_void,
    pub p_viewport_state: *const c_void,
    pub p_rasterization_state: *const c_void,
    pub p_multisample_state: *const c_void,
    pub p_depth_stencil_state: *const c_void,
    pub p_color_blend_state: *const c_void,
    pub p_dynamic_state: *const c_void,
    pub layout: u64,
    pub render_pass: u64,
    pub subpass: u32,
    pub base_pipeline_handle: u64,
    pub base_pipeline_index: i32,
}

#[repr(C)]
pub struct VkPipelineInputAssemblyStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    /// `VkPrimitiveTopology` — 0 POINT_LIST, 1 LINE_LIST, 2 LINE_STRIP, 3 TRIANGLE_LIST, 4 TRIANGLE_STRIP.
    pub topology: i32,
    pub primitive_restart_enable: u32,
}

#[repr(C)]
pub struct VkAttachmentDescription {
    pub flags: VkFlags,
    pub format: i32,
    pub samples: i32,
    pub load_op: i32,
    pub store_op: i32,
    pub stencil_load_op: i32,
    pub stencil_store_op: i32,
    pub initial_layout: i32,
    pub final_layout: i32,
}

#[repr(C)]
pub struct VkRenderPassCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub attachment_count: u32,
    pub p_attachments: *const VkAttachmentDescription,
    pub subpass_count: u32,
    pub p_subpasses: *const c_void,
    pub dependency_count: u32,
    pub p_dependencies: *const c_void,
}

#[repr(C)]
pub struct VkFramebufferCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub render_pass: u64,
    pub attachment_count: u32,
    pub p_attachments: *const u64,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
}

/// `VkClearValue` is a 16-byte union; the color clear path reads it as `float32[4]`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkClearValue {
    pub float32: [f32; 4],
}

#[repr(C)]
pub struct VkRenderPassBeginInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub render_pass: u64,
    pub framebuffer: u64,
    pub render_area: VkRect2D,
    pub clear_value_count: u32,
    pub p_clear_values: *const VkClearValue,
}

// ---- dynamic rendering (VK_KHR_dynamic_rendering / core 1.3) --------------------------------------
// A dynamic-rendering pass carries its attachments inline (no VkRenderPass/VkFramebuffer object). Layout
// from vk.xml; the same sType values for the core and `KHR` aliases.

/// `VK_STRUCTURE_TYPE_RENDERING_INFO`.
pub const VK_STRUCTURE_TYPE_RENDERING_INFO: i32 = 1_000_044_000;
/// `VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO` (a graphics-pipeline pNext carrying color formats).
pub const VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO: i32 = 1_000_044_002;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES` (the feature pNext in `...Features2`).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES: i32 = 1_000_044_003;
/// `VK_ATTACHMENT_STORE_OP_STORE` (a dynamic-rendering attachment stores its result when its storeOp is this).
pub const VK_ATTACHMENT_STORE_OP_STORE: i32 = 0;

/// `VkRenderingAttachmentInfo` — one inline color/depth attachment of a dynamic-rendering pass.
#[repr(C)]
pub struct VkRenderingAttachmentInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub image_view: u64,
    pub image_layout: i32,
    pub resolve_mode: i32,
    pub resolve_image_view: u64,
    pub resolve_image_layout: i32,
    pub load_op: i32,
    pub store_op: i32,
    pub clear_value: VkClearValue,
}

/// `VkRenderingInfo` — the `vkCmdBeginRendering` argument (render area + inline color/depth attachments).
#[repr(C)]
pub struct VkRenderingInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub render_area: VkRect2D,
    pub layer_count: u32,
    pub view_mask: u32,
    pub color_attachment_count: u32,
    pub p_color_attachments: *const VkRenderingAttachmentInfo,
    pub p_depth_attachment: *const VkRenderingAttachmentInfo,
    pub p_stencil_attachment: *const VkRenderingAttachmentInfo,
}

/// `VkPipelineRenderingCreateInfo` — a graphics-pipeline pNext giving the color/depth formats a
/// dynamic-rendering pipeline (null `renderPass`) targets.
#[repr(C)]
pub struct VkPipelineRenderingCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub view_mask: u32,
    pub color_attachment_count: u32,
    pub p_color_attachment_formats: *const i32,
    pub depth_attachment_format: i32,
    pub stencil_attachment_format: i32,
}

/// `VkStencilOpState` — one face's stencil test + operation set. `failOp`/`passOp`/`depthFailOp` are
/// `VkStencilOp` values whose numbering (KEEP=0, ZERO=1, REPLACE=2, INCREMENT_AND_CLAMP=3,
/// DECREMENT_AND_CLAMP=4, INVERT=5, INCREMENT_AND_WRAP=6, DECREMENT_AND_WRAP=7) matches the neutral
/// `hl_gpu` `stencil_op::*` constants verbatim; `compareOp` is a `VkCompareOp` (NEVER=0 … ALWAYS=7) that
/// matches `compare::*` verbatim.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkStencilOpState {
    pub fail_op: i32,
    pub pass_op: i32,
    pub depth_fail_op: i32,
    pub compare_op: i32,
    pub compare_mask: u32,
    pub write_mask: u32,
    pub reference: u32,
}

/// `VkPipelineDepthStencilStateCreateInfo` — the depth/stencil fixed-function state of a graphics
/// pipeline. The full struct is declared: `depthTestEnable`/`depthWriteEnable`/`depthCompareOp` thread to
/// the neutral [`DepthState`] depth test, and `stencilTestEnable` + `front`/`back` (`VkStencilOpState`)
/// thread to its per-face stencil state (the executor honors both — wgpu `StencilState` + the CPU oracle's
/// `Depth24PlusStencil8` stencil plane). `depthBoundsTestEnable` + `min/maxDepthBounds` are NOT modeled (no
/// neutral field expresses depth bounds), but the struct is declared in full so `front`/`back` are read at
/// their correct offsets.
#[repr(C)]
pub struct VkPipelineDepthStencilStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub depth_test_enable: VkBool32,
    pub depth_write_enable: VkBool32,
    pub depth_compare_op: i32,
    pub depth_bounds_test_enable: VkBool32,
    pub stencil_test_enable: VkBool32,
    pub front: VkStencilOpState,
    pub back: VkStencilOpState,
    pub min_depth_bounds: f32,
    pub max_depth_bounds: f32,
}

/// `VkPipelineColorBlendAttachmentState` — the per-color-target fixed-function blend state. All fields are
/// read: `blendEnable` gates whether the target composites (vs. overwrites), and the src/dst factors + ops
/// (each a `VkBlendFactor` / `VkBlendOp`) are translated onto the neutral `hl_gpu` blend wire numbering by
/// `parse_color_blend_state`. `colorWriteMask` is the last field, so this is the full struct.
#[repr(C)]
pub struct VkPipelineColorBlendAttachmentState {
    pub blend_enable: VkBool32,
    pub src_color_blend_factor: i32,
    pub dst_color_blend_factor: i32,
    pub color_blend_op: i32,
    pub src_alpha_blend_factor: i32,
    pub dst_alpha_blend_factor: i32,
    pub alpha_blend_op: i32,
    pub color_write_mask: VkFlags,
}

/// `VkPipelineColorBlendStateCreateInfo` — the color-blend fixed-function state of a graphics pipeline.
/// The first attachment's blend AND `colorWriteMask` are threaded (the software rasterizer applies one
/// blend + one write mask to all targets), so the struct is truncated after `pAttachments`:
/// `logicOp`/`blendConstants` are NOT modeled and no field past this prefix is ever accessed. A null
/// pointer / `blendEnable = VK_FALSE` => no blend (an opaque overwrite); an absent state => `colorWriteMask`
/// defaults to `0xf` (write all channels).
#[repr(C)]
pub struct VkPipelineColorBlendStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub logic_op_enable: VkBool32,
    pub logic_op: i32,
    pub attachment_count: u32,
    pub p_attachments: *const VkPipelineColorBlendAttachmentState,
    // Remaining field (blendConstants[4]) is NOT modeled and is never read through this pointer.
}

/// `VkPipelineMultisampleStateCreateInfo` — the multisample fixed-function state of a graphics pipeline.
/// Only `rasterizationSamples` is read (a `VkSampleCountFlagBits` whose bit VALUE is the sample count:
/// `_1_BIT`=1, `_2_BIT`=2, `_4_BIT`=4, …), threaded to [`RenderPipelineDesc::sample_count`] so an MSAA
/// pipeline rasterizes multisampled. The struct is truncated after `rasterizationSamples`: the remaining
/// fields (sampleShadingEnable, minSampleShading, pSampleMask, alphaToCoverage/OneEnable) are NOT modeled and
/// are never read through this pointer. A null pointer / `_1_BIT` => single-sample.
#[repr(C)]
pub struct VkPipelineMultisampleStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub rasterization_samples: i32,
    // Remaining fields (sampleShadingEnable, minSampleShading, pSampleMask, alphaToCoverageEnable,
    // alphaToOneEnable) are NOT modeled and are never read through this pointer.
}

/// `VkPipelineRasterizationStateCreateInfo` — the rasterization fixed-function state of a graphics
/// pipeline. Only `cullMode` + `frontFace` are read (threaded to the neutral `RenderPipelineDesc::cull` /
/// `front_face`, honored by wgpu's `PrimitiveState` and the CPU oracle's face-cull). The struct is
/// truncated after `frontFace`: `polygonMode` (line/point fill), `depthClampEnable`,
/// `rasterizerDiscardEnable`, and the depthBias / lineWidth tail are NOT expressible in the neutral
/// pipeline and are never read through this pointer.
#[repr(C)]
pub struct VkPipelineRasterizationStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub depth_clamp_enable: VkBool32,
    pub rasterizer_discard_enable: VkBool32,
    pub polygon_mode: i32,
    /// `VkCullModeFlags` — 0 NONE, 1 FRONT_BIT, 2 BACK_BIT, 3 FRONT_AND_BACK.
    pub cull_mode: VkFlags,
    /// `VkFrontFace` — 0 COUNTER_CLOCKWISE, 1 CLOCKWISE.
    pub front_face: i32,
    // Remaining fields (depthBiasEnable, depthBiasConstantFactor, depthBiasClamp, depthBiasSlopeFactor,
    // lineWidth) are NOT modeled and are never read through this pointer.
}

/// `VkPhysicalDeviceDynamicRenderingFeatures` — the feature pNext `vkGetPhysicalDeviceFeatures2` fills to
/// advertise `dynamicRendering = VK_TRUE` (really backed by the `cmd_begin_rendering` lowering).
#[repr(C)]
pub struct VkPhysicalDeviceDynamicRenderingFeatures {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub dynamic_rendering: VkBool32,
}

// ---- bind-memory-2 / memory-requirements-2 (core 1.1 / VK_KHR_bind_memory2 + get_memory_requirements2)
// Each aggregate wraps the v1 arguments behind a `{ sType, pNext }` header; the `...2` entry points read
// these and delegate to the identical v1 body. Layout from vk.xml.

/// `VkBindBufferMemoryInfo` — one `vkBindBufferMemory2` binding.
#[repr(C)]
pub struct VkBindBufferMemoryInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub buffer: u64,
    pub memory: u64,
    pub memory_offset: VkDeviceSize,
}

/// `VkBindImageMemoryInfo` — one `vkBindImageMemory2` binding.
#[repr(C)]
pub struct VkBindImageMemoryInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub image: u64,
    pub memory: u64,
    pub memory_offset: VkDeviceSize,
}

/// `VkBufferMemoryRequirementsInfo2` — the `vkGetBufferMemoryRequirements2` input (the queried buffer).
#[repr(C)]
pub struct VkBufferMemoryRequirementsInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub buffer: u64,
}

/// `VkImageMemoryRequirementsInfo2` — the `vkGetImageMemoryRequirements2` input (the queried image).
#[repr(C)]
pub struct VkImageMemoryRequirementsInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub image: u64,
}

/// `VkMemoryRequirements2` — the `...Requirements2` output (base `VkMemoryRequirements` + preserved chain).
#[repr(C)]
pub struct VkMemoryRequirements2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub memory_requirements: VkMemoryRequirements,
}

#[repr(C)]
pub struct VkSemaphoreCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
pub struct VkSwapchainCreateInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub surface: u64,
    pub min_image_count: u32,
    pub image_format: i32,
    pub image_color_space: i32,
    pub image_extent: VkExtent2D,
    pub image_array_layers: u32,
    pub image_usage: VkFlags,
    pub image_sharing_mode: i32,
    pub queue_family_index_count: u32,
    pub p_queue_family_indices: *const u32,
    pub pre_transform: VkFlags,
    pub composite_alpha: VkFlags,
    pub present_mode: i32,
    pub clipped: VkBool32,
    pub old_swapchain: u64,
}

/// `VkWaylandSurfaceCreateInfoKHR` (`VK_KHR_wayland_surface`) — the app's native wayland handles the ICD
/// captures at `vkCreateWaylandSurfaceKHR`: `display` is the app's `wl_display*` and `surface` its
/// `wl_surface*`, both on the app's OWN `libwayland-client` connection. The shim records these (never
/// dereferences them here) so `vkQueuePresentKHR` can marshal the presented frame onto that `wl_surface`.
#[repr(C)]
pub struct VkWaylandSurfaceCreateInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    /// `struct wl_display*` — the app's connection.
    pub display: *mut c_void,
    /// `struct wl_surface*` — the app window's surface.
    pub surface: *mut c_void,
}

#[repr(C)]
pub struct VkPresentInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub wait_semaphore_count: u32,
    pub p_wait_semaphores: *const u64,
    pub swapchain_count: u32,
    pub p_swapchains: *const u64,
    pub p_image_indices: *const u32,
    pub p_results: *mut i32,
}

// ==================================================================================================
// transfer-path structs (buffer/image copies, blits, clears, pipeline barriers) — layout from vk.xml
// ==================================================================================================

/// `VK_IMAGE_ASPECT_COLOR_BIT` (the only aspect the software oracle materializes).
pub const VK_IMAGE_ASPECT_COLOR_BIT: u32 = 0x0000_0001;
/// `VK_FILTER_LINEAR` (`vkCmdBlitImage` filter; `VK_FILTER_NEAREST` = 0).
pub const VK_FILTER_LINEAR: i32 = 1;

/// A `pNext`-chain input node header (`{ sType, pNext }`, const) — every extension struct begins with it.
#[repr(C)]
pub struct VkBaseInStructure {
    pub s_type: i32,
    pub p_next: *const VkBaseInStructure,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkOffset3D {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// `VkImageSubresourceLayers` — the (aspect, mip, layers) a copy/blit region reads or writes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkImageSubresourceLayers {
    pub aspect_mask: VkFlags,
    pub mip_level: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

#[repr(C)]
pub struct VkBufferCopy {
    pub src_offset: VkDeviceSize,
    pub dst_offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

#[repr(C)]
pub struct VkBufferImageCopy {
    pub buffer_offset: VkDeviceSize,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub image_subresource: VkImageSubresourceLayers,
    pub image_offset: VkOffset3D,
    pub image_extent: VkExtent3D,
}

#[repr(C)]
pub struct VkImageCopy {
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offset: VkOffset3D,
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offset: VkOffset3D,
    pub extent: VkExtent3D,
}

#[repr(C)]
pub struct VkImageBlit {
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offsets: [VkOffset3D; 2],
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offsets: [VkOffset3D; 2],
}

/// `VkClearColorValue` is a 16-byte union; the color-clear path reads it as `float32[4]`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkClearColorValue {
    pub float32: [f32; 4],
}

/// `VkClearDepthStencilValue` — the depth (`f32`) + stencil (`u32`) a `vkCmdClearDepthStencilImage` clears to.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkClearDepthStencilValue {
    pub depth: f32,
    pub stencil: u32,
}

#[repr(C)]
pub struct VkClearAttachment {
    pub aspect_mask: VkFlags,
    pub color_attachment: u32,
    pub clear_value: VkClearValue,
}

#[repr(C)]
pub struct VkClearRect {
    pub rect: VkRect2D,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

// ---- the `...2` copy/blit wrappers (core 1.3 / VK_KHR_copy_commands2) ----------------------------
// Each `vkCmd*2` takes a single `Vk*Info2` aggregate whose `pRegions` array holds `Vk*Copy2`/`Vk*Blit2`
// structs — the same region payload as the v1 command, prefixed by a `{ sType, pNext }` node header. The
// `...2` entry points read these and delegate to the identical v1 lowering. Layout from vk.xml.

/// `VkBufferCopy2` — one `vkCmdCopyBuffer2` region ( `VkBufferCopy` + chain header).
#[repr(C)]
pub struct VkBufferCopy2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_offset: VkDeviceSize,
    pub dst_offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

/// `VkCopyBufferInfo2` — the `vkCmdCopyBuffer2` argument aggregate.
#[repr(C)]
pub struct VkCopyBufferInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_buffer: u64,
    pub dst_buffer: u64,
    pub region_count: u32,
    pub p_regions: *const VkBufferCopy2,
}

/// `VkBufferImageCopy2` — one `vkCmdCopyBufferToImage2` region (`VkBufferImageCopy` + chain header).
#[repr(C)]
pub struct VkBufferImageCopy2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub buffer_offset: VkDeviceSize,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub image_subresource: VkImageSubresourceLayers,
    pub image_offset: VkOffset3D,
    pub image_extent: VkExtent3D,
}

/// `VkCopyBufferToImageInfo2` — the `vkCmdCopyBufferToImage2` argument aggregate.
#[repr(C)]
pub struct VkCopyBufferToImageInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_buffer: u64,
    pub dst_image: u64,
    pub dst_image_layout: i32,
    pub region_count: u32,
    pub p_regions: *const VkBufferImageCopy2,
}

/// `VkImageBlit2` — one `vkCmdBlitImage2` region (`VkImageBlit` + chain header).
#[repr(C)]
pub struct VkImageBlit2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offsets: [VkOffset3D; 2],
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offsets: [VkOffset3D; 2],
}

/// `VkBlitImageInfo2` — the `vkCmdBlitImage2` argument aggregate.
#[repr(C)]
pub struct VkBlitImageInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_image: u64,
    pub src_image_layout: i32,
    pub dst_image: u64,
    pub dst_image_layout: i32,
    pub region_count: u32,
    pub p_regions: *const VkImageBlit2,
    pub filter: i32,
}

/// `VkImageMemoryBarrier` (legacy / core 1.0) — an image's `oldLayout → newLayout` transition.
#[repr(C)]
pub struct VkImageMemoryBarrier {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_access_mask: VkFlags,
    pub dst_access_mask: VkFlags,
    pub old_layout: i32,
    pub new_layout: i32,
    pub src_queue_family_index: u32,
    pub dst_queue_family_index: u32,
    pub image: u64,
    pub subresource_range: VkImageSubresourceRange,
}

/// `VkImageMemoryBarrier2` (synchronization2 / core 1.3) — per-barrier 64-bit stage/access masks.
#[repr(C)]
pub struct VkImageMemoryBarrier2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_stage_mask: u64,
    pub src_access_mask: u64,
    pub dst_stage_mask: u64,
    pub dst_access_mask: u64,
    pub old_layout: i32,
    pub new_layout: i32,
    pub src_queue_family_index: u32,
    pub dst_queue_family_index: u32,
    pub image: u64,
    pub subresource_range: VkImageSubresourceRange,
}

/// `VkDependencyInfo` — the `vkCmdPipelineBarrier2` argument aggregating the sync2 barrier arrays.
#[repr(C)]
pub struct VkDependencyInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub dependency_flags: VkFlags,
    pub memory_barrier_count: u32,
    pub p_memory_barriers: *const c_void,
    pub buffer_memory_barrier_count: u32,
    pub p_buffer_memory_barriers: *const c_void,
    pub image_memory_barrier_count: u32,
    pub p_image_memory_barriers: *const VkImageMemoryBarrier2,
}

// ==================================================================================================
// sync + query object structs (events, timeline semaphores, query pools) — layout from vk.xml
// ==================================================================================================

/// `VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO` (the pNext node selecting a timeline semaphore).
pub const VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO: i32 = 1_000_207_002;
/// `VK_SEMAPHORE_TYPE_TIMELINE` (`VkSemaphoreTypeCreateInfo::semaphoreType`; BINARY = 0).
pub const VK_SEMAPHORE_TYPE_TIMELINE: i32 = 1;

/// `VkQueryResultFlagBits` (stable ABI).
pub const VK_QUERY_RESULT_64_BIT: u32 = 0x1;
pub const VK_QUERY_RESULT_WAIT_BIT: u32 = 0x2;
pub const VK_QUERY_RESULT_WITH_AVAILABILITY_BIT: u32 = 0x4;
pub const VK_QUERY_RESULT_PARTIAL_BIT: u32 = 0x8;
/// `VK_SEMAPHORE_WAIT_ANY_BIT` (`VkSemaphoreWaitFlags`).
pub const VK_SEMAPHORE_WAIT_ANY_BIT: u32 = 0x1;

#[repr(C)]
pub struct VkEventCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
}

/// `VkSemaphoreTypeCreateInfo` — a `VkSemaphoreCreateInfo` pNext selecting BINARY/TIMELINE + initial value.
#[repr(C)]
pub struct VkSemaphoreTypeCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore_type: i32,
    pub initial_value: u64,
}

#[repr(C)]
pub struct VkSemaphoreSignalInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore: u64,
    pub value: u64,
}

/// `VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO` — the `VkSubmitInfo` pNext carrying the per-wait /
/// per-signal timeline VALUES paired positionally with `VkSubmitInfo::pWaitSemaphores` / `pSignalSemaphores`.
pub const VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO: i32 = 1_000_207_003;

/// `VkTimelineSemaphoreSubmitInfo` (`VK_KHR_timeline_semaphore`) — a `VkSubmitInfo` pNext supplying the
/// timeline counter values for the submit's wait/signal semaphore arrays. `pSignalSemaphoreValues[i]` is
/// the value queue completion advances `VkSubmitInfo::pSignalSemaphores[i]` to (ignored for a binary
/// semaphore); `pWaitSemaphoreValues[i]` is the value the submit waits `pWaitSemaphores[i]` to reach.
#[repr(C)]
pub struct VkTimelineSemaphoreSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub wait_semaphore_value_count: u32,
    pub p_wait_semaphore_values: *const u64,
    pub signal_semaphore_value_count: u32,
    pub p_signal_semaphore_values: *const u64,
}

#[repr(C)]
pub struct VkSemaphoreWaitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub semaphore_count: u32,
    pub p_semaphores: *const u64,
    pub p_values: *const u64,
}

#[repr(C)]
pub struct VkQueryPoolCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub query_type: i32,
    pub query_count: u32,
    pub pipeline_statistics: VkFlags,
}

// ==================================================================================================
// descriptor-update-template / pipeline-cache / device-queue-2 / WSI-surface structs (from vk.xml)
// ==================================================================================================

/// `VkDescriptorUpdateTemplateEntry` — one entry mapping a `pData` slice (`offset`/`stride`) to a
/// `(dstBinding, dstArrayElement)` of a given descriptor class.
#[repr(C)]
pub struct VkDescriptorUpdateTemplateEntry {
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
    pub descriptor_type: i32,
    pub offset: usize,
    pub stride: usize,
}

/// `VkDescriptorUpdateTemplateCreateInfo` — the entry table + template type (`DESCRIPTOR_SET` = 0).
#[repr(C)]
pub struct VkDescriptorUpdateTemplateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub descriptor_update_entry_count: u32,
    pub p_descriptor_update_entries: *const VkDescriptorUpdateTemplateEntry,
    pub template_type: i32,
    pub descriptor_set_layout: u64,
    pub pipeline_bind_point: i32,
    pub pipeline_layout: u64,
    pub set: u32,
}

/// `VkPipelineCacheCreateInfo` — the optional serialized `initialData` blob a cache is restored from.
#[repr(C)]
pub struct VkPipelineCacheCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub initial_data_size: usize,
    pub p_initial_data: *const c_void,
}

/// `VkDeviceQueueInfo2` — the `(flags, queueFamilyIndex, queueIndex)` a `vkGetDeviceQueue2` retrieves.
#[repr(C)]
pub struct VkDeviceQueueInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub queue_family_index: u32,
    pub queue_index: u32,
}

/// `VkImageSubresource` — the `(aspect, mip, arrayLayer)` a `vkGetImageSubresourceLayout` queries.
#[repr(C)]
pub struct VkImageSubresource {
    pub aspect_mask: VkFlags,
    pub mip_level: u32,
    pub array_layer: u32,
}

/// `VkSubresourceLayout` — the linear byte layout (offset/size/pitches) written back to the app.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSubresourceLayout {
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
    pub row_pitch: VkDeviceSize,
    pub array_pitch: VkDeviceSize,
    pub depth_pitch: VkDeviceSize,
}

/// `VkSurfaceCapabilitiesKHR` (`VK_KHR_surface`) — the min/max image count, extents, transforms + usage.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSurfaceCapabilitiesKHR {
    pub min_image_count: u32,
    pub max_image_count: u32,
    pub current_extent: VkExtent2D,
    pub min_image_extent: VkExtent2D,
    pub max_image_extent: VkExtent2D,
    pub max_image_array_layers: u32,
    pub supported_transforms: VkFlags,
    pub current_transform: VkFlags,
    pub supported_composite_alpha: VkFlags,
    pub supported_usage_flags: VkFlags,
}

/// `VkSurfaceFormatKHR` — one `(format, colorSpace)` a surface presents.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkSurfaceFormatKHR {
    pub format: i32,
    pub color_space: i32,
}

// ==================================================================================================
// maintenance1-5 / private-data / host-image-copy / promoted-`...2` structs (layout from vk.xml)
// ==================================================================================================

/// `VkDescriptorSetLayoutSupport` — `vkGetDescriptorSetLayoutSupport` writes `supported` (whether a set
/// of the queried layout can be created). The bring-up model accepts any layout within the reported
/// limits, so this is `VK_TRUE`.
#[repr(C)]
pub struct VkDescriptorSetLayoutSupport {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub supported: VkBool32,
}

/// `VkDeviceBufferMemoryRequirements` (`vkGetDeviceBufferMemoryRequirements` input) — the buffer's
/// create info, from which the requirements are derived WITHOUT creating the buffer.
#[repr(C)]
pub struct VkDeviceBufferMemoryRequirements {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub p_create_info: *const VkBufferCreateInfo,
}

/// `VkDeviceImageMemoryRequirements` (`vkGetDeviceImageMemoryRequirements` input) — the image's create
/// info + the plane aspect (single-plane here).
#[repr(C)]
pub struct VkDeviceImageMemoryRequirements {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub p_create_info: *const VkImageCreateInfo,
    pub plane_aspect: i32,
}

/// `VkImageSubresource2(KHR)` — the `vkGetImageSubresourceLayout2` input (a `VkImageSubresource` + chain).
#[repr(C)]
pub struct VkImageSubresource2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub image_subresource: VkImageSubresource,
}

/// `VkSubresourceLayout2(KHR)` — the `vkGetImageSubresourceLayout2` output (base layout + chain).
#[repr(C)]
pub struct VkSubresourceLayout2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub subresource_layout: VkSubresourceLayout,
}

/// `VkImageCopy2` — one `vkCmdCopyImage2` region (`VkImageCopy` + chain header).
#[repr(C)]
pub struct VkImageCopy2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_subresource: VkImageSubresourceLayers,
    pub src_offset: VkOffset3D,
    pub dst_subresource: VkImageSubresourceLayers,
    pub dst_offset: VkOffset3D,
    pub extent: VkExtent3D,
}

/// `VkCopyImageInfo2` — the `vkCmdCopyImage2` argument aggregate.
#[repr(C)]
pub struct VkCopyImageInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_image: u64,
    pub src_image_layout: i32,
    pub dst_image: u64,
    pub dst_image_layout: i32,
    pub region_count: u32,
    pub p_regions: *const VkImageCopy2,
}

/// `VkCopyImageToBufferInfo2` — the `vkCmdCopyImageToBuffer2` argument aggregate (reuses `VkBufferImageCopy2`).
#[repr(C)]
pub struct VkCopyImageToBufferInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_image: u64,
    pub src_image_layout: i32,
    pub dst_buffer: u64,
    pub region_count: u32,
    pub p_regions: *const VkBufferImageCopy2,
}

/// `VkCommandBufferBeginInfo` — the `vkBeginCommandBuffer` info. Only `flags` is consumed by the model
/// (the `ONE_TIME_SUBMIT` bit decides whether a completed submit returns the buffer to `Executable` for
/// re-submission or leaves it non-resubmittable); `pInheritanceInfo` is validated-then-ignored.
#[repr(C)]
pub struct VkCommandBufferBeginInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub p_inheritance_info: *const c_void,
}

/// `VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT` — the buffer is recorded for a single submission.
pub const VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: VkFlags = 0x0000_0001;

/// `VkCommandBufferSubmitInfo` — one command buffer of a `vkQueueSubmit2` batch (a dispatchable handle).
#[repr(C)]
pub struct VkCommandBufferSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub command_buffer: *mut c_void,
    pub device_mask: u32,
}

/// `VkSemaphoreSubmitInfo` (sync2) — one wait/signal entry of a `vkQueueSubmit2` batch. Carries the
/// timeline `value` inline (unlike v1's side-array `VkTimelineSemaphoreSubmitInfo`).
#[repr(C)]
pub struct VkSemaphoreSubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub semaphore: u64,
    pub value: u64,
    pub stage_mask: u64,
    pub device_index: u32,
}

/// `VkSubmitInfo2` — the `vkQueueSubmit2` batch (sync2). The command-buffer array and the signal
/// semaphore infos (for queue-side timeline signals) are consumed; the wait infos are validated-then-
/// ignored (the synchronous model has already satisfied them).
#[repr(C)]
pub struct VkSubmitInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub wait_semaphore_info_count: u32,
    pub p_wait_semaphore_infos: *const c_void,
    pub command_buffer_info_count: u32,
    pub p_command_buffer_infos: *const VkCommandBufferSubmitInfo,
    pub signal_semaphore_info_count: u32,
    pub p_signal_semaphore_infos: *const c_void,
}

/// `VkMappedMemoryRange` — one range for `vkFlush/InvalidateMappedMemoryRanges` (`size == VK_WHOLE_SIZE`
/// means from `offset` to the end of the allocation).
#[repr(C)]
pub struct VkMappedMemoryRange {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub memory: u64,
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

/// `VkMemoryMapInfo(KHR)` — the `vkMapMemory2` argument aggregate.
#[repr(C)]
pub struct VkMemoryMapInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub memory: u64,
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
}

/// `VkMemoryUnmapInfo(KHR)` — the `vkUnmapMemory2` argument aggregate.
#[repr(C)]
pub struct VkMemoryUnmapInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub memory: u64,
}

/// `VkPhysicalDeviceImageFormatInfo2` — the `vkGetPhysicalDeviceImageFormatProperties2` input.
#[repr(C)]
pub struct VkPhysicalDeviceImageFormatInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub format: i32,
    pub image_type: i32,
    pub tiling: i32,
    pub usage: VkFlags,
    pub flags: VkFlags,
}

/// `VkImageFormatProperties2` — the `...ImageFormatProperties2` output (base props + preserved chain).
#[repr(C)]
pub struct VkImageFormatProperties2 {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub image_format_properties: VkImageFormatProperties,
}

/// `VkAttachmentDescription2` — one attachment of a `VkRenderPassCreateInfo2` (`+ sType/pNext` vs v1).
#[repr(C)]
pub struct VkAttachmentDescription2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub format: i32,
    pub samples: i32,
    pub load_op: i32,
    pub store_op: i32,
    pub stencil_load_op: i32,
    pub stencil_store_op: i32,
    pub initial_layout: i32,
    pub final_layout: i32,
}

/// `VkRenderPassCreateInfo2` — the `vkCreateRenderPass2` argument (only the attachment table is read for
/// the bring-up single-target clear/format bookkeeping).
#[repr(C)]
pub struct VkRenderPassCreateInfo2 {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub attachment_count: u32,
    pub p_attachments: *const VkAttachmentDescription2,
    pub subpass_count: u32,
    pub p_subpasses: *const c_void,
    pub dependency_count: u32,
    pub p_dependencies: *const c_void,
    pub correlated_view_mask_count: u32,
    pub p_correlated_view_masks: *const u32,
}

/// `VkCalibratedTimestampInfoKHR` — one queried time domain of `vkGetCalibratedTimestamps`.
#[repr(C)]
pub struct VkCalibratedTimestampInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub time_domain: i32,
}

/// `VK_TIME_DOMAIN_DEVICE_KHR` — the only calibrateable time domain the modeled device reports.
pub const VK_TIME_DOMAIN_DEVICE_KHR: i32 = 0;
