//! The hl-shim-vk object model + the "hl Metal (Vulkan)" physical-device description.
//!
//! Object model mirrors MoltenVK's (`MVKInstance` / `MVKPhysicalDevice` / `MVKDevice` / `MVKQueue` /
//! `MVKCommandPool`): a small dispatch/ownership graph. Each **dispatchable** object's ICD state is
//! wrapped in [`crate::handle::Dispatchable`] (loader-magic slot in field 0). The property *values*
//! (Apple vendor id `0x106b`, unified memory, one graphics+compute+transfer queue family) are ported
//! from MoltenVK's Apple-silicon reporting in `MVKDevice.mm`; the exact struct layouts come from
//! `ash::vk`.

use ash::vk;

// ---- dispatchable object inner-state (behind crate::handle::Dispatchable) -------------------------

/// `VkInstance` state: the requested API version (used for the version-5 compatibility check) and the
/// single physical device we expose.
pub struct Instance {
    pub app_api_version: u32,
    pub physical_device: crate::types::VkPhysicalDevice,
}

/// `VkPhysicalDevice` state — a back-pointer to the owning instance (raw; the instance outlives it).
pub struct PhysicalDevice {
    pub instance: crate::types::VkInstance,
}

/// `VkDevice` state: its physical device and the lone queue it owns.
pub struct Device {
    pub physical_device: crate::types::VkPhysicalDevice,
    pub queue: crate::types::VkQueue,
}

/// `VkQueue` state — which family/index it was retrieved as.
pub struct Queue {
    pub family_index: u32,
    pub queue_index: u32,
}

/// `VkCommandBuffer` state — the pool it was allocated from (kept for future free/reset wiring).
pub struct CommandBuffer {
    pub pool: crate::types::VkCommandPool,
}

/// Non-dispatchable `VkCommandPool` payload.
pub struct CommandPool {
    pub queue_family_index: u32,
}

// ---- the reported device -------------------------------------------------------------------------

/// The Vulkan API version we advertise (device + `vkEnumerateInstanceVersion`). **Vulkan 1.4.0.**
/// Truthfulness gate (gui_vk_capability_truth): advertise only the version whose mandatory core is
/// actually backed. The **entire mandatory core for Vulkan 1.0–1.4 now has real bodies** — the full
/// 234-command core spec surface (`crate::capability` core census 137+28+13+37+19 = 234/234) — so 1.4 is
/// honestly selectable. A 2.0+ request is refused with `VK_ERROR_INCOMPATIBLE_DRIVER`. Feature *bits*
/// (queried via `vkGetPhysicalDeviceFeatures2`) are reported truthfully per what each command actually
/// materializes; an app enables only the features it detects, so advertising 1.4 never over-promises.
// `ash` 0.38 (headers 1.3.281) predates the `API_VERSION_1_4` constant, so spell it explicitly.
pub const HL_API_VERSION: u32 = vk::make_api_version(0, 1, 4, 0);
/// Apple's PCI vendor id, as MoltenVK reports (`kAppleVendorId` in MVKDevice.mm).
pub const APPLE_VENDOR_ID: u32 = 0x106b;
/// `driverVersion` — hl's own driver revision (packed like an api version), increment 1.
pub const HL_DRIVER_VERSION: u32 = vk::make_api_version(0, 0, 1, 0);
/// The single queue family we expose (graphics + compute + transfer, one queue).
pub const QUEUE_FAMILY_INDEX: u32 = 0;

/// The physical-device name a real loader/app reads back. The plan's required identity.
pub const DEVICE_NAME: &str = "hl Metal (Vulkan)";

/// Fill a `VkPhysicalDeviceProperties`. Ported from MoltenVK's Apple-silicon reporting: integrated
/// GPU, Apple vendor id, unified-memory limits. Limits are a plausible Metal-class subset (the long
/// tail stays zero this increment; later increments refine them from the real device).
pub fn physical_device_properties() -> vk::PhysicalDeviceProperties {
    let mut p = vk::PhysicalDeviceProperties {
        api_version: HL_API_VERSION,
        driver_version: HL_DRIVER_VERSION,
        vendor_id: APPLE_VENDOR_ID,
        device_id: 0xdd_00_0001,
        device_type: vk::PhysicalDeviceType::INTEGRATED_GPU,
        ..Default::default()
    };
    // deviceName: NUL-terminated C string in the fixed-size array.
    let name = DEVICE_NAME.as_bytes();
    for (dst, &b) in p.device_name.iter_mut().zip(name.iter()) {
        *dst = b as core::ffi::c_char;
    }
    // pipelineCacheUUID: a stable hl-specific tag (bytes "hlMetalVulkan\0\0\0").
    let uuid = b"hlMetalVulkan\0\0\0";
    p.pipeline_cache_uuid.copy_from_slice(&uuid[..16]);
    p.limits = physical_device_limits();
    p
}

/// The full Apple-GPU-class `VkPhysicalDeviceLimits`. Ported from MoltenVK `MVKPhysicalDevice::init*Limits`
/// (`GPUObjects/MVKDevice.mm`) with its Metal-feature-derived values (`maxPerStageBufferCount = 31`,
/// `maxPerStageSamplerCount = 16`, `maxPerStageTextureCount = 128`, `maxTextureDimension = 16384`,
/// `mtlBufferAlignment = 256`). A modern app (wgpu-hal / Zed) reads EVERY limit to build its own limit
/// table and validate pipeline/resource creation — leaving the long tail at zero made wgpu reject the
/// device, so this fills the whole struct with truthful Metal-class values.
fn physical_device_limits() -> vk::PhysicalDeviceLimits {
    // Metal per-stage argument-table sizes (MVKPhysicalDevice `_metalFeatures`, Apple GPU class).
    let per_stage_buffers = 31u32;
    let per_stage_samplers = 16u32;
    let per_stage_textures = 128u32;
    let per_stage_storage_images = 128u32;
    let per_stage_resources = per_stage_buffers + per_stage_textures; // 159
    let dim = 16384u32; // maxTextureDimension (Apple3+)
    // We support single- and 4x-multisample color (the resolve path materializes 4x).
    let color_samples = vk::SampleCountFlags::TYPE_1 | vk::SampleCountFlags::TYPE_4;
    let one_sample = vk::SampleCountFlags::TYPE_1;
    vk::PhysicalDeviceLimits {
        max_image_dimension1_d: dim,
        max_image_dimension2_d: dim,
        max_image_dimension3_d: 2048,
        max_image_dimension_cube: dim,
        max_image_array_layers: 2048,
        max_texel_buffer_elements: dim * 4096, // maxImageDimension2D * 4Ki
        max_uniform_buffer_range: 65536,
        max_storage_buffer_range: u32::MAX,
        max_push_constants_size: 4096,
        max_memory_allocation_count: 4096,
        max_sampler_allocation_count: 4096,
        buffer_image_granularity: 1,
        sparse_address_space_size: 0,
        max_bound_descriptor_sets: 8,
        max_per_stage_descriptor_samplers: per_stage_samplers,
        max_per_stage_descriptor_uniform_buffers: per_stage_buffers,
        max_per_stage_descriptor_storage_buffers: per_stage_buffers,
        max_per_stage_descriptor_sampled_images: per_stage_textures,
        max_per_stage_descriptor_storage_images: per_stage_storage_images,
        max_per_stage_descriptor_input_attachments: per_stage_textures,
        max_per_stage_resources: per_stage_resources,
        // Vulkan sums per-stage across the 5 graphics stages (MoltenVK uses *5).
        max_descriptor_set_samplers: per_stage_samplers * 5,
        max_descriptor_set_uniform_buffers: per_stage_buffers * 5,
        max_descriptor_set_uniform_buffers_dynamic: per_stage_buffers * 5,
        max_descriptor_set_storage_buffers: per_stage_buffers * 5,
        max_descriptor_set_storage_buffers_dynamic: per_stage_buffers * 5,
        max_descriptor_set_sampled_images: per_stage_textures * 5,
        max_descriptor_set_storage_images: per_stage_storage_images * 5,
        max_descriptor_set_input_attachments: per_stage_textures * 5,
        max_vertex_input_attributes: 31,
        max_vertex_input_bindings: 31,
        max_vertex_input_attribute_offset: 4095,
        max_vertex_input_binding_stride: 4096,
        max_vertex_output_components: 124,
        max_tessellation_generation_level: 0,
        max_tessellation_patch_size: 0,
        max_geometry_shader_invocations: 0,
        max_fragment_input_components: 124,
        max_fragment_output_attachments: 8,
        max_fragment_dual_src_attachments: 1,
        max_fragment_combined_output_resources: per_stage_resources,
        max_compute_shared_memory_size: 32768,
        max_compute_work_group_count: [65535, 65535, 65535],
        max_compute_work_group_invocations: 1024,
        max_compute_work_group_size: [1024, 1024, 1024],
        sub_pixel_precision_bits: 4,
        sub_texel_precision_bits: 4,
        mipmap_precision_bits: 4,
        max_draw_indexed_index_value: u32::MAX,
        max_draw_indirect_count: u32::MAX,
        max_sampler_lod_bias: 15.999,
        max_sampler_anisotropy: 16.0,
        max_viewports: 16,
        max_viewport_dimensions: [dim, dim],
        viewport_bounds_range: [-(2.0 * dim as f32), 2.0 * dim as f32 - 1.0],
        viewport_sub_pixel_bits: 0,
        min_memory_map_alignment: 256,
        min_texel_buffer_offset_alignment: 16,
        min_uniform_buffer_offset_alignment: 256,
        min_storage_buffer_offset_alignment: 16,
        min_texel_offset: -8,
        max_texel_offset: 7,
        min_texel_gather_offset: -8,
        max_texel_gather_offset: 7,
        min_interpolation_offset: -0.5,
        max_interpolation_offset: 0.5,
        sub_pixel_interpolation_offset_bits: 4,
        max_framebuffer_width: dim,
        max_framebuffer_height: dim,
        max_framebuffer_layers: 2048,
        framebuffer_color_sample_counts: color_samples,
        framebuffer_depth_sample_counts: color_samples,
        framebuffer_stencil_sample_counts: color_samples,
        framebuffer_no_attachments_sample_counts: color_samples,
        max_color_attachments: 8,
        sampled_image_color_sample_counts: color_samples,
        sampled_image_integer_sample_counts: one_sample,
        sampled_image_depth_sample_counts: color_samples,
        sampled_image_stencil_sample_counts: color_samples,
        storage_image_sample_counts: one_sample,
        max_sample_mask_words: 1,
        timestamp_compute_and_graphics: vk::TRUE,
        timestamp_period: 1.0,
        max_clip_distances: 8,
        max_cull_distances: 8,
        max_combined_clip_and_cull_distances: 8,
        discrete_queue_priorities: 2,
        point_size_range: [1.0, 511.0],
        line_width_range: [1.0, 1.0],
        point_size_granularity: 0.0,
        line_width_granularity: 0.0,
        strict_lines: vk::FALSE,
        standard_sample_locations: vk::TRUE,
        optimal_buffer_copy_offset_alignment: 256,
        optimal_buffer_copy_row_pitch_alignment: 1,
        non_coherent_atom_size: 256,
        ..Default::default()
    }
}

/// Fill a `VkPhysicalDeviceFeatures` with only guarantees implemented across every executor path.
pub fn physical_device_features() -> vk::PhysicalDeviceFeatures {
    vk::PhysicalDeviceFeatures {
        // Descriptor, vertex/index and translated shader accesses do not yet share Vulkan's required
        // zero-read/discard-write robustness semantics. Advertising this would be unsafe.
        robust_buffer_access: vk::FALSE,
        full_draw_index_uint32: vk::TRUE,
        image_cube_array: vk::TRUE,
        independent_blend: vk::TRUE,
        sampler_anisotropy: vk::TRUE,
        texture_compression_bc: vk::TRUE,
        shader_int16: vk::TRUE,
        ..Default::default()
    }
}

/// Fill a `VkPhysicalDeviceMemoryProperties` — Apple-silicon **unified memory** (MoltenVK model in
/// MVKDevice.mm): a single DEVICE_LOCAL heap, one memory type that is
/// DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT (CPU and GPU share it).
pub fn memory_properties() -> vk::PhysicalDeviceMemoryProperties {
    let mut m = vk::PhysicalDeviceMemoryProperties::default();
    m.memory_heap_count = 1;
    m.memory_heaps[0] = vk::MemoryHeap {
        size: 8 * 1024 * 1024 * 1024, // 8 GiB reported (unified, shared with system RAM)
        flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
    };
    m.memory_type_count = 1;
    m.memory_types[0] = vk::MemoryType {
        property_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL
            | vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        heap_index: 0,
    };
    m
}

/// The one queue family: graphics + compute + transfer, a single queue. Matches the unified Metal
/// command-queue model MoltenVK exposes.
pub fn queue_family_properties() -> vk::QueueFamilyProperties {
    vk::QueueFamilyProperties {
        queue_flags: vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER,
        queue_count: 1,
        timestamp_valid_bits: 64,
        min_image_transfer_granularity: vk::Extent3D {
            width: 1,
            height: 1,
            depth: 1,
        },
    }
}
