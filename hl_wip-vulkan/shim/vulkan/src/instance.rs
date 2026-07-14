//! Instance / physical-device bring-up: `vkCreateInstance`, physical-device enumeration + the property
//! queries a probe (loader / wgpu-hal) reads to accept the "hl Metal (Vulkan)" device.
//!
//! Pure object model + property write-back — no IR is emitted here. Property *values* are ported from
//! `hl-shim-vk/src/state.rs` (MoltenVK Apple-silicon reporting); the structs are the clean-room
//! `#[repr(C)]` layouts in [`crate::types`]. Handles are loader-magic'd dispatchable tokens
//! ([`crate::state`]).

use core::ffi::{c_char, c_void};

use hl_vulkan::model::capability::{self, ExtensionProp};
use hl_vulkan::service::create;

use crate::state::with;
use crate::types::*;

/// Write `s` (nul-terminated) into a fixed-size `c_char` array, zero-filling and truncating to fit.
fn write_name(dst: &mut [c_char], s: &str) {
    for d in dst.iter_mut() {
        *d = 0;
    }
    let b = s.as_bytes();
    let n = b.len().min(dst.len().saturating_sub(1));
    for i in 0..n {
        dst[i] = b[i] as c_char;
    }
}

/// The Vulkan two-call enumeration idiom: with `p_data` NULL, write the item count; otherwise copy up
/// to `*p_count` items (truncating to `VK_INCOMPLETE` if the caller's buffer is smaller).
///
/// # Safety
/// `p_count` must be a valid `*mut u32`; `p_data`, if non-NULL, must point to `*p_count` writable `T`s.
unsafe fn write_enumeration<T: Copy>(items: &[T], p_count: *mut u32, p_data: *mut T) -> VkResult {
    let Some(count) = p_count.as_mut() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_data.is_null() {
        *count = items.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(items.len());
    core::ptr::copy_nonoverlapping(items.as_ptr(), p_data, n);
    *count = n as u32;
    if n < items.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

/// Build a `VkExtensionProperties` from an advertised extension descriptor.
fn ext_prop(e: &ExtensionProp) -> VkExtensionProperties {
    let mut p = VkExtensionProperties { extension_name: [0; VK_MAX_EXTENSION_NAME_SIZE], spec_version: e.spec_version };
    write_name(&mut p.extension_name, e.name);
    p
}

// ==================================================================================================
// instance
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateInstance(
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_instance: *mut *mut c_void,
) -> VkResult {
    if p_instance.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    // The app-requested API version (default to what we advertise if the ApplicationInfo is absent).
    let app_api = unsafe {
        (p_create_info as *const VkInstanceCreateInfo)
            .as_ref()
            .and_then(|ci| ci.p_application_info.as_ref())
            .map(|ai| ai.api_version)
            .filter(|&v| v != 0)
            .unwrap_or(HL_API_VERSION)
    };
    with(|s| s.instance = Some(create::create_instance(app_api)));
    let token = Dispatchable::new(());
    unsafe { *p_instance = token };
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyInstance(instance: *mut c_void, _p_allocator: *const c_void) {
    with(|s| s.instance = None);
    unsafe { Dispatchable::<()>::free(instance) };
}

#[no_mangle]
pub extern "C" fn vkEnumerateInstanceVersion(p_api_version: *mut u32) -> VkResult {
    if p_api_version.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    unsafe { *p_api_version = HL_API_VERSION };
    VK_SUCCESS
}

/// Instance extensions the ICD really backs (allow-list, not the whole `vk.xml`): the WSI base
/// `VK_KHR_surface` + `VK_KHR_get_physical_device_properties2` (the `...2` queries below). A real app
/// gates its init on these being enumerated, so this is the key unblock. Sourced from
/// [`capability::INSTANCE_EXTENSIONS`].
#[no_mangle]
pub extern "C" fn vkEnumerateInstanceExtensionProperties(
    _p_layer_name: *const c_char,
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    let exts: Vec<VkExtensionProperties> = capability::INSTANCE_EXTENSIONS.iter().map(ext_prop).collect();
    unsafe { write_enumeration(&exts, p_property_count, p_properties as *mut VkExtensionProperties) }
}

/// The ICD exposes no layers (layers are discovered from layer manifests, never the driver).
#[no_mangle]
pub extern "C" fn vkEnumerateInstanceLayerProperties(
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    unsafe { write_enumeration::<VkLayerProperties>(&[], p_property_count, p_properties as *mut VkLayerProperties) }
}

// ==================================================================================================
// physical device
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkEnumeratePhysicalDevices(
    _instance: *mut c_void,
    p_physical_device_count: *mut u32,
    p_physical_devices: *mut *mut c_void,
) -> VkResult {
    if p_physical_device_count.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if p_physical_devices.is_null() {
        unsafe { *p_physical_device_count = 1 };
        return VK_SUCCESS;
    }
    let cap = unsafe { *p_physical_device_count };
    if cap < 1 {
        unsafe { *p_physical_device_count = 0 };
        return VK_INCOMPLETE;
    }
    let pd = with(|s| s.phys_dev_handle());
    unsafe {
        *p_physical_devices = pd;
        *p_physical_device_count = 1;
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties(
    _physical_device: *mut c_void,
    p_properties: *mut c_void,
) -> VkResult {
    let Some(p) = (unsafe { (p_properties as *mut VkPhysicalDeviceProperties).as_mut() }) else {
        return VK_SUCCESS;
    };
    let desc = with(|s| s.physical_device());
    p.api_version = desc.api_version;
    p.driver_version = desc.driver_version;
    p.vendor_id = desc.vendor_id;
    p.device_id = desc.device_id;
    p.device_type = desc.device_type as i32;
    write_name(&mut p.device_name, &desc.name);
    p.pipeline_cache_uuid = desc.pipeline_cache_uuid;
    p.limits = metal_limits();
    p.sparse_properties = VkPhysicalDeviceSparseProperties::default();
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures(_physical_device: *mut c_void, p_features: *mut c_void) {
    let Some(f) = (unsafe { (p_features as *mut VkPhysicalDeviceFeatures).as_mut() }) else {
        return;
    };
    f.bits = [VK_FALSE; 55];
    // The conservative feature subset guaranteed across every executor path (indices into the vk.xml
    // `VkPhysicalDeviceFeatures` order): fullDrawIndexUint32(1), imageCubeArray(2), independentBlend(3),
    // samplerAnisotropy(19), textureCompressionBC(22), shaderInt16(41). Ported from `state.rs`.
    for i in [1usize, 2, 3, 19, 22, 41] {
        f.bits[i] = VK_TRUE;
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties(
    _physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    let Some(m) = (unsafe { (p_memory_properties as *mut VkPhysicalDeviceMemoryProperties).as_mut() })
    else {
        return;
    };
    let heap_bytes = with(|s| s.physical_device().memory_heap_bytes);
    m.memory_types = [VkMemoryType::default(); VK_MAX_MEMORY_TYPES];
    m.memory_heaps = [VkMemoryHeap::default(); VK_MAX_MEMORY_HEAPS];
    // One Apple-unified heap (DEVICE_LOCAL), one memory type: DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT.
    m.memory_heap_count = 1;
    m.memory_heaps[0] = VkMemoryHeap { size: heap_bytes, flags: 0x1 };
    m.memory_type_count = 1;
    m.memory_types[0] = VkMemoryType { property_flags: 0x1 | 0x2 | 0x4, heap_index: 0 };
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties(
    _physical_device: *mut c_void,
    p_queue_family_property_count: *mut u32,
    p_queue_family_properties: *mut c_void,
) {
    if p_queue_family_property_count.is_null() {
        return;
    }
    if p_queue_family_properties.is_null() {
        unsafe { *p_queue_family_property_count = 1 };
        return;
    }
    if unsafe { *p_queue_family_property_count } < 1 {
        unsafe { *p_queue_family_property_count = 0 };
        return;
    }
    let qf = with(|s| s.physical_device().queue_family);
    let out = p_queue_family_properties as *mut VkQueueFamilyProperties;
    unsafe {
        *out = VkQueueFamilyProperties {
            queue_flags: qf.queue_flags,
            queue_count: qf.queue_count,
            timestamp_valid_bits: qf.timestamp_valid_bits,
            min_image_transfer_granularity: VkExtent3D { width: 1, height: 1, depth: 1 },
        };
        *p_queue_family_property_count = 1;
    }
}

/// Device extensions the ICD really backs: `VK_KHR_swapchain` (the present path). Nothing unbacked is
/// advertised. Sourced from [`capability::DEVICE_EXTENSIONS`].
#[no_mangle]
pub extern "C" fn vkEnumerateDeviceExtensionProperties(
    _physical_device: *mut c_void,
    _p_layer_name: *const c_char,
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    let exts: Vec<VkExtensionProperties> = capability::DEVICE_EXTENSIONS.iter().map(ext_prop).collect();
    unsafe { write_enumeration(&exts, p_property_count, p_properties as *mut VkExtensionProperties) }
}

/// Deprecated device-layer enumeration — always empty (spec: return instance layers or none).
#[no_mangle]
pub extern "C" fn vkEnumerateDeviceLayerProperties(
    _physical_device: *mut c_void,
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    unsafe { write_enumeration::<VkLayerProperties>(&[], p_property_count, p_properties as *mut VkLayerProperties) }
}

// ==================================================================================================
// format + image-format queries
// ==================================================================================================

/// `vkGetPhysicalDeviceFormatProperties` — the truthful per-format feature masks (color formats:
/// color-attachment/blend/sampled/storage/blit/transfer; depth: depth-stencil-attachment/sampled/
/// transfer; vertex float: vertex-buffer). Sourced from [`capability::format_features`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties(
    _physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_format_properties as *mut VkFormatProperties).as_mut() }) else {
        return;
    };
    let ff = capability::format_features(format as u32);
    out.linear_tiling_features = ff.linear_tiling;
    out.optimal_tiling_features = ff.optimal_tiling;
    out.buffer_features = ff.buffer;
}

/// `vkGetPhysicalDeviceImageFormatProperties` — the creation limits for a `(format, type, tiling, …)`
/// combination, or `VK_ERROR_FORMAT_NOT_SUPPORTED` when not creatable (spec §12.3). Reports the
/// supported 2D-optimal color subset with the device limits; everything else is truthfully unsupported.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties(
    _physical_device: *mut c_void,
    format: i32,
    image_type: i32,
    tiling: i32,
    _usage: VkFlags,
    _flags: VkFlags,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    const VK_ERROR_FORMAT_NOT_SUPPORTED: VkResult = -11;
    const VK_IMAGE_TYPE_2D: i32 = 1;
    const VK_IMAGE_TILING_OPTIMAL: i32 = 0;
    let Some(out) = (unsafe { (p_image_format_properties as *mut VkImageFormatProperties).as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !capability::image_format_supported(format as u32)
        || image_type != VK_IMAGE_TYPE_2D
        || tiling != VK_IMAGE_TILING_OPTIMAL
    {
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    let dim = with(|s| s.physical_device().limits.max_image_dimension_2d);
    *out = VkImageFormatProperties {
        max_extent: VkExtent3D { width: dim, height: dim, depth: 1 },
        max_mip_levels: 1 + (dim as f32).log2() as u32,
        max_array_layers: 2048,
        sample_counts: 0x1 | 0x4, // TYPE_1 | TYPE_4
        max_resource_size: 1 << 31, // 2 GiB (the executor residency budget)
    };
    VK_SUCCESS
}

// ==================================================================================================
// the `...2` physical-device queries (VK_KHR_get_physical_device_properties2 / core 1.1)
// ==================================================================================================

/// Write a NUL-terminated C string into a fixed `[c_char; N]` array (truncating).
fn write_cstr(dst: &mut [c_char], s: &str) {
    write_name(dst, s);
}

/// `vkGetPhysicalDeviceProperties2` — the base properties + the pNext payloads apps read back
/// (driver name/info, maintenance3 descriptor ceilings). Each node's sType/pNext is preserved.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties2(
    physical_device: *mut c_void,
    p_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_properties as *mut VkPhysicalDeviceProperties2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceProperties(physical_device, &mut out.properties as *mut _ as *mut c_void);
    let mut node = out.p_next as *mut VkBaseOutStructure;
    while let Some(n) = unsafe { node.as_mut() } {
        if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES {
            if let Some(d) = unsafe { (node as *mut VkPhysicalDeviceDriverProperties).as_mut() } {
                d.driver_id = 8; // VK_DRIVER_ID_MESA_LLVMPIPE — a valid id (hl has no registered one)
                write_cstr(&mut d.driver_name, "hl");
                write_cstr(&mut d.driver_info, "hl Metal (Vulkan) 0.1");
                d.conformance_version = VkConformanceVersion { major: 1, minor: 4, subminor: 0, patch: 0 };
            }
        } else if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES {
            if let Some(m) = unsafe { (node as *mut VkPhysicalDeviceMaintenance3Properties).as_mut() } {
                m.max_per_set_descriptors = 1_000_000;
                m.max_memory_allocation_size = 1 << 31; // 2 GiB
            }
        }
        node = n.p_next;
    }
}

/// `vkGetPhysicalDeviceFeatures2` — the base 1.0 feature set, plus the promoted-feature pNext structs we
/// really back. `VkPhysicalDeviceDynamicRenderingFeatures::dynamicRendering` is set `VK_TRUE` (backed by
/// the `cmd_begin_rendering` lowering + the advertised `VK_KHR_dynamic_rendering` device extension). Every
/// OTHER promoted-feature struct is left as the app zero-initialized it (`VK_FALSE`) — we advertise no
/// feature we do not implement. The chain is preserved.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures2(
    physical_device: *mut c_void,
    p_features: *mut c_void,
) {
    let Some(out) = (unsafe { (p_features as *mut VkPhysicalDeviceFeatures2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceFeatures(physical_device, &mut out.features as *mut _ as *mut c_void);
    let mut node = out.p_next as *mut VkBaseOutStructure;
    while let Some(n) = unsafe { node.as_mut() } {
        if n.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES {
            if let Some(f) = unsafe { (node as *mut VkPhysicalDeviceDynamicRenderingFeatures).as_mut() } {
                f.dynamic_rendering = VK_TRUE;
            }
        }
        node = n.p_next;
    }
}

/// `vkGetPhysicalDeviceMemoryProperties2` — delegates to the 1.0 memory-properties fill.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties2(
    physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_memory_properties as *mut VkPhysicalDeviceMemoryProperties2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceMemoryProperties(physical_device, &mut out.memory_properties as *mut _ as *mut c_void);
}

/// `vkGetPhysicalDeviceQueueFamilyProperties2` — delegates to the 1.0 queue-family fill.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties2(
    physical_device: *mut c_void,
    p_queue_family_property_count: *mut u32,
    p_queue_family_properties: *mut c_void,
) {
    if p_queue_family_property_count.is_null() {
        return;
    }
    if p_queue_family_properties.is_null() {
        vkGetPhysicalDeviceQueueFamilyProperties(physical_device, p_queue_family_property_count, core::ptr::null_mut());
        return;
    }
    if unsafe { *p_queue_family_property_count } < 1 {
        unsafe { *p_queue_family_property_count = 0 };
        return;
    }
    let out = p_queue_family_properties as *mut VkQueueFamilyProperties2;
    if let Some(o) = unsafe { out.as_mut() } {
        vkGetPhysicalDeviceQueueFamilyProperties(
            physical_device,
            p_queue_family_property_count,
            &mut o.queue_family_properties as *mut _ as *mut c_void,
        );
    }
    unsafe { *p_queue_family_property_count = 1 };
}

/// `vkGetPhysicalDeviceFormatProperties2` — delegates to the 1.0 per-format feature fill.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties2(
    physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    let Some(out) = (unsafe { (p_format_properties as *mut VkFormatProperties2).as_mut() }) else {
        return;
    };
    vkGetPhysicalDeviceFormatProperties(physical_device, format, &mut out.format_properties as *mut _ as *mut c_void);
}

// ==================================================================================================
// the `...2KHR` aliases (VK_KHR_get_physical_device_properties2) — delegate to the promoted-core bodies
// ==================================================================================================
// Each is the pre-promotion `KHR` name of the identical core-1.1 query; it forwards verbatim so an app /
// loader that resolves the `KHR` spelling gets the same truthful answer as the core entry point.

/// `vkGetPhysicalDeviceProperties2KHR` — alias of the core [`vkGetPhysicalDeviceProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties2KHR(physical_device: *mut c_void, p_properties: *mut c_void) {
    vkGetPhysicalDeviceProperties2(physical_device, p_properties)
}

/// `vkGetPhysicalDeviceFeatures2KHR` — alias of the core [`vkGetPhysicalDeviceFeatures2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures2KHR(physical_device: *mut c_void, p_features: *mut c_void) {
    vkGetPhysicalDeviceFeatures2(physical_device, p_features)
}

/// `vkGetPhysicalDeviceMemoryProperties2KHR` — alias of the core [`vkGetPhysicalDeviceMemoryProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties2KHR(
    physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    vkGetPhysicalDeviceMemoryProperties2(physical_device, p_memory_properties)
}

/// `vkGetPhysicalDeviceQueueFamilyProperties2KHR` — alias of the core
/// [`vkGetPhysicalDeviceQueueFamilyProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties2KHR(
    physical_device: *mut c_void,
    p_queue_family_property_count: *mut u32,
    p_queue_family_properties: *mut c_void,
) {
    vkGetPhysicalDeviceQueueFamilyProperties2(
        physical_device,
        p_queue_family_property_count,
        p_queue_family_properties,
    )
}

/// `vkGetPhysicalDeviceFormatProperties2KHR` — alias of the core [`vkGetPhysicalDeviceFormatProperties2`].
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties2KHR(
    physical_device: *mut c_void,
    format: i32,
    p_format_properties: *mut c_void,
) {
    vkGetPhysicalDeviceFormatProperties2(physical_device, format, p_format_properties)
}

/// `vkGetPhysicalDeviceImageFormatProperties2` — read the `VkPhysicalDeviceImageFormatInfo2` aggregate
/// and fill the base `VkImageFormatProperties` via the v1 body (the pNext chain is preserved).
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties2(
    physical_device: *mut c_void,
    p_image_format_info: *const c_void,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    let Some(info) = (unsafe { (p_image_format_info as *const VkPhysicalDeviceImageFormatInfo2).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some(out) = (unsafe { (p_image_format_properties as *mut VkImageFormatProperties2).as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    vkGetPhysicalDeviceImageFormatProperties(
        physical_device,
        info.format,
        info.image_type,
        info.tiling,
        info.usage,
        info.flags,
        &mut out.image_format_properties as *mut _ as *mut c_void,
    )
}

/// `vkGetPhysicalDeviceImageFormatProperties2KHR` — the `VK_KHR_get_physical_device_properties2` alias.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties2KHR(
    physical_device: *mut c_void,
    p_image_format_info: *const c_void,
    p_image_format_properties: *mut c_void,
) -> VkResult {
    vkGetPhysicalDeviceImageFormatProperties2(physical_device, p_image_format_info, p_image_format_properties)
}

/// The full Apple-GPU-class `VkPhysicalDeviceLimits` (all 106 fields), ported verbatim from
/// `hl-shim-vk/src/state.rs::physical_device_limits` (MoltenVK Metal-feature-derived values).
fn metal_limits() -> VkPhysicalDeviceLimits {
    let dim = 16384u32;
    let per_stage_buffers = 31u32;
    let per_stage_samplers = 16u32;
    let per_stage_textures = 128u32;
    let per_stage_storage_images = 128u32;
    let per_stage_resources = per_stage_buffers + per_stage_textures; // 159
    let color_samples: VkFlags = 0x1 | 0x4; // TYPE_1 | TYPE_4
    let one_sample: VkFlags = 0x1;
    VkPhysicalDeviceLimits {
        max_image_dimension_1d: dim,
        max_image_dimension_2d: dim,
        max_image_dimension_3d: 2048,
        max_image_dimension_cube: dim,
        max_image_array_layers: 2048,
        max_texel_buffer_elements: dim * 4096,
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
        max_tessellation_control_per_vertex_input_components: 0,
        max_tessellation_control_per_vertex_output_components: 0,
        max_tessellation_control_per_patch_output_components: 0,
        max_tessellation_control_total_output_components: 0,
        max_tessellation_evaluation_input_components: 0,
        max_tessellation_evaluation_output_components: 0,
        max_geometry_shader_invocations: 0,
        max_geometry_input_components: 0,
        max_geometry_output_components: 0,
        max_geometry_output_vertices: 0,
        max_geometry_total_output_components: 0,
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
        timestamp_compute_and_graphics: VK_TRUE,
        timestamp_period: 1.0,
        max_clip_distances: 8,
        max_cull_distances: 8,
        max_combined_clip_and_cull_distances: 8,
        discrete_queue_priorities: 2,
        point_size_range: [1.0, 511.0],
        line_width_range: [1.0, 1.0],
        point_size_granularity: 0.0,
        line_width_granularity: 0.0,
        strict_lines: VK_FALSE,
        standard_sample_locations: VK_TRUE,
        optimal_buffer_copy_offset_alignment: 256,
        optimal_buffer_copy_row_pitch_alignment: 1,
        non_coherent_atom_size: 256,
    }
}
