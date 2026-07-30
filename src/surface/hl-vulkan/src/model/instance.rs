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
    /// this ICD advertises ([`HL_API_VERSION`], Vulkan 1.3) is the caller's to reject with
    /// `VK_ERROR_INCOMPATIBLE_DRIVER`; this constructor just records it.
    pub fn new(app_api_version: u32) -> Self {
        Self {
            app_api_version,
            physical_device: PhysicalDeviceDesc::hl_default(),
        }
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
    /// `VkPhysicalDeviceIDProperties::deviceUUID` — stable identity of this physical device. Tools and
    /// applications key caches and device matching on it, so an all-zero value makes the device
    /// unidentifiable and indistinguishable from every other driver reporting zeros.
    pub device_uuid: [u8; 16],
    /// `VkPhysicalDeviceIDProperties::driverUUID` — stable identity of this driver build.
    pub driver_uuid: [u8; 16],
    pub limits: Limits,
    /// Size in bytes of the single unified DEVICE_LOCAL heap (Apple unified memory, shared with system
    /// RAM). Backs every advertised memory type — see [`PhysicalDeviceDesc::memory_types`].
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

// ---- advertised memory-type / memory-heap table (`vkGetPhysicalDeviceMemoryProperties`) ----------
//
// `VkMemoryPropertyFlagBits` (the low bits the shim writes into `VkMemoryType::propertyFlags`).
/// `VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT`.
pub const MEMORY_PROPERTY_DEVICE_LOCAL: u32 = 0x1;
/// `VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT` (mappable via `vkMapMemory`).
pub const MEMORY_PROPERTY_HOST_VISIBLE: u32 = 0x2;
/// `VK_MEMORY_PROPERTY_HOST_COHERENT_BIT`.
pub const MEMORY_PROPERTY_HOST_COHERENT: u32 = 0x4;
/// `VK_MEMORY_PROPERTY_HOST_CACHED_BIT`.
pub const MEMORY_PROPERTY_HOST_CACHED: u32 = 0x8;
/// `VK_MEMORY_HEAP_DEVICE_LOCAL_BIT`.
pub const MEMORY_HEAP_DEVICE_LOCAL: u32 = 0x1;

/// One row of the advertised `VkPhysicalDeviceMemoryProperties::memoryTypes` table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryTypeDesc {
    /// `VkMemoryPropertyFlags` (a combination of the `MEMORY_PROPERTY_*` bits above).
    pub property_flags: u32,
    /// Index into the `memoryHeaps` table (always `0` here — one unified heap).
    pub heap_index: u32,
}

/// One row of the advertised `VkPhysicalDeviceMemoryProperties::memoryHeaps` table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryHeapDesc {
    pub size: u64,
    /// `VkMemoryHeapFlags`.
    pub flags: u32,
}

impl PhysicalDeviceDesc {
    /// The advertised memory heaps: a single unified `DEVICE_LOCAL` heap. All our memory is host RAM
    /// behind the executor (shared with system RAM), modeled as one Apple-unified device-local heap.
    pub fn memory_heaps(&self) -> Vec<MemoryHeapDesc> {
        vec![MemoryHeapDesc {
            size: self.memory_heap_bytes,
            flags: MEMORY_HEAP_DEVICE_LOCAL,
        }]
    }

    /// The advertised memory types — the STANDARD software-Vulkan layout (mirrors lavapipe / lvp): a
    /// device-local-only type, a device-local + host-visible-coherent type, a host-visible-coherent
    /// type, and a host-visible-coherent-cached type.
    ///
    /// Every type is honest: all our memory is host RAM behind the executor, so every `HOST_VISIBLE`
    /// type here IS mappable via `vkMapMemory`, and the `DEVICE_LOCAL`-only type (index 0) is simply
    /// never mapped (gpu-alloc / wgpu only map host-visible blocks). Exposing the standard set — rather
    /// than a single combined `DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT` type — lets wgpu-hal's gpu-alloc
    /// pick a distinct type per usage (GPU-only resources → type 0, uploads → type 2, readback →
    /// type 3) instead of routing every resource through one combined type.
    pub fn memory_types(&self) -> Vec<MemoryTypeDesc> {
        vec![
            // 0: DEVICE_LOCAL — GPU-resident resources (vertex/index/storage, textures). Never mapped.
            MemoryTypeDesc {
                property_flags: MEMORY_PROPERTY_DEVICE_LOCAL,
                heap_index: 0,
            },
            // 1: DEVICE_LOCAL | HOST_VISIBLE | HOST_COHERENT — unified type (the Apple-silicon default).
            MemoryTypeDesc {
                property_flags: MEMORY_PROPERTY_DEVICE_LOCAL
                    | MEMORY_PROPERTY_HOST_VISIBLE
                    | MEMORY_PROPERTY_HOST_COHERENT,
                heap_index: 0,
            },
            // 2: HOST_VISIBLE | HOST_COHERENT — plain upload staging.
            MemoryTypeDesc {
                property_flags: MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT,
                heap_index: 0,
            },
            // 3: HOST_VISIBLE | HOST_COHERENT | HOST_CACHED — readback / download staging.
            MemoryTypeDesc {
                property_flags: MEMORY_PROPERTY_HOST_VISIBLE
                    | MEMORY_PROPERTY_HOST_COHERENT
                    | MEMORY_PROPERTY_HOST_CACHED,
                heap_index: 0,
            },
        ]
    }

    /// The `VkMemoryRequirements::memoryTypeBits` every resource reports: one bit per advertised memory
    /// type. All our memory is host RAM, so ANY resource can be backed by ANY advertised type; reporting
    /// the full set lets gpu-alloc choose the right type per usage (a host-access buffer lands in a
    /// `HOST_VISIBLE` type, a GPU-only resource in the `DEVICE_LOCAL` type). Kept in lock-step with
    /// [`Self::memory_types`] so a resource is never denied the type gpu-alloc wants.
    pub fn all_memory_type_bits(&self) -> u32 {
        let n = self.memory_types().len() as u32;
        debug_assert!(
            n <= 32,
            "more than 32 memory types cannot fit in memoryTypeBits"
        );
        // 4 types -> 0b1111. (n is 4 here; `1u32 << 32` would overflow, guarded by the assert above.)
        (1u32 << n) - 1
    }
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
            device_id: 0xdd00_0001,
            device_type: DEVICE_TYPE_INTEGRATED_GPU,
            pipeline_cache_uuid: *b"hlMetalVulkan\0\0\0",
            device_uuid: *b"hl-Metal-device0",
            driver_uuid: *b"hl-Vulkan-driver",
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

#[cfg(test)]
mod memory_layout_tests {
    use super::*;

    /// The advertised memory types/heaps are the STANDARD software-Vulkan set (mirrors lavapipe): valid
    /// heap indices, a DEVICE_LOCAL heap, a HOST_VISIBLE|HOST_COHERENT type, a mappable type, and
    /// `memoryTypeBits` covering exactly the advertised types. This is the source the shim's
    /// `vkGetPhysicalDeviceMemoryProperties` reads; a regression here is what made wgpu-hal's gpu-alloc
    /// mis-serve Zed's small (~12 MB) working set as an out-of-memory device loss.
    #[test]
    fn advertised_memory_types_and_heaps_are_the_standard_set() {
        let pd = PhysicalDeviceDesc::hl_default();
        let heaps = pd.memory_heaps();
        let types = pd.memory_types();

        // Heaps: at least one, all sized, and a DEVICE_LOCAL heap present.
        assert!(!heaps.is_empty());
        assert!(
            heaps.iter().all(|h| h.size > 0),
            "every heap must have a real size"
        );
        assert!(
            heaps
                .iter()
                .any(|h| h.flags & MEMORY_HEAP_DEVICE_LOCAL != 0),
            "a DEVICE_LOCAL heap"
        );

        // Types: the standard multi-type layout, every type points at a valid heap.
        assert!(
            types.len() >= 2,
            "must advertise more than a single combined type"
        );
        for t in &types {
            assert!(
                (t.heap_index as usize) < heaps.len(),
                "type references an out-of-range heap"
            );
        }

        // A plain HOST_VISIBLE|HOST_COHERENT upload type exists (gpu-alloc's UPLOAD choice).
        let hv_hc = MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT;
        assert!(
            types.iter().any(|t| t.property_flags & hv_hc == hv_hc),
            "HOST_VISIBLE|HOST_COHERENT type"
        );
        // A mappable (HOST_VISIBLE) type exists — every HOST_VISIBLE type IS mappable via vkMapMemory.
        assert!(
            types
                .iter()
                .any(|t| t.property_flags & MEMORY_PROPERTY_HOST_VISIBLE != 0),
            "a mappable type"
        );
        // A HOST_CACHED type (downloads) and a DEVICE_LOCAL type (GPU-only) exist.
        assert!(
            types
                .iter()
                .any(|t| t.property_flags & MEMORY_PROPERTY_HOST_CACHED != 0),
            "a HOST_CACHED type"
        );
        assert!(
            types
                .iter()
                .any(|t| t.property_flags & MEMORY_PROPERTY_DEVICE_LOCAL != 0),
            "a DEVICE_LOCAL type"
        );

        // memoryTypeBits covers exactly the advertised types (all memory is host RAM: any type can back
        // any resource), and exposes more than the single core-1.0 type (index 0).
        let bits = pd.all_memory_type_bits();
        assert_eq!(bits, (1u32 << types.len()) - 1);
        assert!(
            bits > 1,
            "must expose more than the single core-1.0 combined type"
        );
    }
}
