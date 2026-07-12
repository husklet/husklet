//! The dd-shim-vk object model + the "dd Metal (Vulkan)" physical-device description.
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

/// The Vulkan API version we advertise (device + `vkEnumerateInstanceVersion`). 1.3.0.
pub const DD_API_VERSION: u32 = vk::API_VERSION_1_3;
/// Apple's PCI vendor id, as MoltenVK reports (`kAppleVendorId` in MVKDevice.mm).
pub const APPLE_VENDOR_ID: u32 = 0x106b;
/// `driverVersion` — dd's own driver revision (packed like an api version), increment 1.
pub const DD_DRIVER_VERSION: u32 = vk::make_api_version(0, 0, 1, 0);
/// The single queue family we expose (graphics + compute + transfer, one queue).
pub const QUEUE_FAMILY_INDEX: u32 = 0;

/// The physical-device name a real loader/app reads back. The plan's required identity.
pub const DEVICE_NAME: &str = "dd Metal (Vulkan)";

/// Fill a `VkPhysicalDeviceProperties`. Ported from MoltenVK's Apple-silicon reporting: integrated
/// GPU, Apple vendor id, unified-memory limits. Limits are a plausible Metal-class subset (the long
/// tail stays zero this increment; later increments refine them from the real device).
pub fn physical_device_properties() -> vk::PhysicalDeviceProperties {
    let mut p = vk::PhysicalDeviceProperties {
        api_version: DD_API_VERSION,
        driver_version: DD_DRIVER_VERSION,
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
    // pipelineCacheUUID: a stable dd-specific tag (bytes "ddMetalVulkan\0\0\0").
    let uuid = b"ddMetalVulkan\0\0\0";
    p.pipeline_cache_uuid.copy_from_slice(&uuid[..16]);
    p.limits = physical_device_limits();
    p
}

/// A plausible Metal-class limits subset (the values apps most commonly sanity-check). Not the whole
/// ~100-field struct — the rest defaults to zero and is refined against the real device later.
fn physical_device_limits() -> vk::PhysicalDeviceLimits {
    vk::PhysicalDeviceLimits {
        max_image_dimension1_d: 16384,
        max_image_dimension2_d: 16384,
        max_image_dimension3_d: 2048,
        max_image_dimension_cube: 16384,
        max_image_array_layers: 2048,
        max_bound_descriptor_sets: 8,
        max_memory_allocation_count: 4096,
        max_uniform_buffer_range: 65536,
        max_storage_buffer_range: u32::MAX,
        max_push_constants_size: 4096,
        max_compute_shared_memory_size: 32768,
        max_compute_work_group_count: [65535, 65535, 65535],
        max_compute_work_group_invocations: 1024,
        max_compute_work_group_size: [1024, 1024, 1024],
        max_color_attachments: 8,
        buffer_image_granularity: 1,
        min_uniform_buffer_offset_alignment: 256,
        min_storage_buffer_offset_alignment: 16,
        max_sampler_anisotropy: 16.0,
        ..Default::default()
    }
}

/// Fill a `VkPhysicalDeviceFeatures` — a conservative "safe subset" on by default (the common set an
/// app expects available). Refined as real bodies land.
pub fn physical_device_features() -> vk::PhysicalDeviceFeatures {
    vk::PhysicalDeviceFeatures {
        robust_buffer_access: vk::TRUE,
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
