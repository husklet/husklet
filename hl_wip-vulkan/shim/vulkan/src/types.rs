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
