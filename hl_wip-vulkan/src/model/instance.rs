//! The `VkInstance` object + the simulated "hl Metal (Vulkan)" **physical device** — the numbers a
//! probe (`vkGetPhysicalDeviceProperties`/`Limits`/`MemoryProperties`, wgpu-hal's limit table) reads
//! back so it accepts the device.
//!
//! Ported from `hl-shim-vk/src/state.rs` (`Instance`, `physical_device_properties`,
//! `physical_device_limits`, `memory_properties`, `queue_family_properties`), whose property *values*
//! (Apple vendor id `0x106b`, unified memory, one graphics+compute+transfer queue family, Metal-class
//! limits) are themselves ported from MoltenVK's Apple-silicon reporting in `MVKDevice.mm`. Pure data;
//! the `ash::vk` structs the real ICD fills are replaced here by plain owned values so the crate needs
//! no `ash` dependency.

use crate::result::{HL_API_VERSION, HL_DRIVER_VERSION};

/// Apple's PCI vendor id, as MoltenVK reports (`kAppleVendorId`).
pub const APPLE_VENDOR_ID: u32 = 0x106b;
/// The single queue family we expose (graphics + compute + transfer, one queue).
pub const QUEUE_FAMILY_INDEX: u32 = 0;
/// The physical-device name a real loader/app reads back.
pub const DEVICE_NAME: &str = "hl Metal (Vulkan)";

/// `VkPhysicalDeviceType::INTEGRATED_GPU` (unified memory) — the stable enum value.
pub const DEVICE_TYPE_INTEGRATED_GPU: u32 = 1;

/// A `VkInstance`: the app-requested API version (for the version compatibility check) and the single
/// physical device it exposes.
#[derive(Clone, PartialEq, Debug)]
pub struct Instance {
    pub app_api_version: u32,
    pub physical_device: PhysicalDeviceDesc,
}

impl Instance {
    /// Create the instance exposing the default hl physical device. A requested API version above what
    /// this ICD advertises ([`HL_API_VERSION`], Vulkan 1.4) is the caller's to reject with
    /// `VK_ERROR_INCOMPATIBLE_DRIVER`; this constructor just records it.
    pub fn new(app_api_version: u32) -> Self {
        Self { app_api_version, physical_device: PhysicalDeviceDesc::hl_default() }
    }
}

/// What `vkGetPhysicalDeviceProperties` / `…Limits` / `…MemoryProperties` / `…QueueFamilyProperties`
/// report for the simulated device (the subset the bring-up path + wgpu-hal actually read).
#[derive(Clone, PartialEq, Debug)]
pub struct PhysicalDeviceDesc {
    pub name: String,
    pub api_version: u32,
    pub driver_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    /// `VkPhysicalDeviceType` (1 = INTEGRATED_GPU).
    pub device_type: u32,
    /// A stable hl-specific `pipelineCacheUUID` tag.
    pub pipeline_cache_uuid: [u8; 16],
    pub limits: Limits,
    /// Single unified DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT heap size in bytes (Apple unified memory).
    pub memory_heap_bytes: u64,
    /// The one queue family (graphics|compute|transfer), one queue.
    pub queue_family: QueueFamily,
}

/// The Metal-class `VkPhysicalDeviceLimits` subset a modern app reads to build its own limit table.
/// Ported from `hl-shim-vk/src/state.rs::physical_device_limits` (MoltenVK Apple-GPU values).
#[derive(Clone, PartialEq, Debug)]
pub struct Limits {
    pub max_image_dimension_2d: u32,
    pub max_image_dimension_3d: u32,
    pub max_uniform_buffer_range: u32,
    pub max_storage_buffer_range: u32,
    pub max_push_constants_size: u32,
    pub max_bound_descriptor_sets: u32,
    pub max_per_stage_descriptor_storage_buffers: u32,
    pub max_per_stage_resources: u32,
    pub max_vertex_input_attributes: u32,
    pub max_vertex_input_bindings: u32,
    pub max_compute_shared_memory_size: u32,
    pub max_compute_work_group_count: [u32; 3],
    pub max_compute_work_group_invocations: u32,
    pub max_compute_work_group_size: [u32; 3],
    pub max_color_attachments: u32,
    pub max_framebuffer_width: u32,
    pub max_framebuffer_height: u32,
    pub min_uniform_buffer_offset_alignment: u64,
    pub min_storage_buffer_offset_alignment: u64,
    pub min_memory_map_alignment: u64,
    pub non_coherent_atom_size: u64,
}

/// The one queue family we expose: graphics + compute + transfer, a single queue.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct QueueFamily {
    /// `VkQueueFlags` (GRAPHICS 1 | COMPUTE 2 | TRANSFER 4 = 7).
    pub queue_flags: u32,
    pub queue_count: u32,
    pub timestamp_valid_bits: u32,
}

impl PhysicalDeviceDesc {
    /// The default hl physical device: presents as an Apple-silicon-class integrated GPU backed by
    /// unified memory, with Metal-class limits.
    pub fn hl_default() -> Self {
        Self {
            name: DEVICE_NAME.into(),
            api_version: HL_API_VERSION,
            driver_version: HL_DRIVER_VERSION,
            vendor_id: APPLE_VENDOR_ID,
            device_id: 0xdd_00_0001,
            device_type: DEVICE_TYPE_INTEGRATED_GPU,
            pipeline_cache_uuid: *b"hlMetalVulkan\0\0\0",
            limits: Limits::metal_class(),
            memory_heap_bytes: 8 * 1024 * 1024 * 1024, // 8 GiB (unified, shared with system RAM)
            queue_family: QueueFamily {
                queue_flags: 0b111, // GRAPHICS | COMPUTE | TRANSFER
                queue_count: 1,
                timestamp_valid_bits: 64,
            },
        }
    }
}

impl Limits {
    /// The Metal-class limit set ported from MoltenVK's Apple-GPU reporting.
    pub fn metal_class() -> Self {
        let dim = 16384u32;
        Self {
            max_image_dimension_2d: dim,
            max_image_dimension_3d: 2048,
            max_uniform_buffer_range: 65536,
            max_storage_buffer_range: u32::MAX,
            max_push_constants_size: 4096,
            max_bound_descriptor_sets: 8,
            max_per_stage_descriptor_storage_buffers: 31,
            max_per_stage_resources: 159,
            max_vertex_input_attributes: 31,
            max_vertex_input_bindings: 31,
            max_compute_shared_memory_size: 32768,
            max_compute_work_group_count: [65535, 65535, 65535],
            max_compute_work_group_invocations: 1024,
            max_compute_work_group_size: [1024, 1024, 1024],
            max_color_attachments: 8,
            max_framebuffer_width: dim,
            max_framebuffer_height: dim,
            min_uniform_buffer_offset_alignment: 256,
            min_storage_buffer_offset_alignment: 16,
            min_memory_map_alignment: 256,
            non_coherent_atom_size: 256,
        }
    }
}
