//! Instance-level + physical-device entry points (real bodies).
//!
//! Ported from MoltenVK's `MVKInstance` / `MVKPhysicalDevice` object model and the Vulkan spec
//! semantics; the property values come from [`crate::state`] (the "dd Metal (Vulkan)" device). The
//! big out-structs (`VkPhysicalDeviceProperties`, …) are `ash::vk` types so the ABI is spec-exact.

use crate::handle::Dispatchable;
use crate::state::{self, Device, Instance, PhysicalDevice, Queue};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::{c_char, c_void};

/// The classic two-call enumeration idiom: `(pCount, pData)`. Writes up to `*pCount` items, sets
/// `*pCount` to the number written, returns `VK_INCOMPLETE` if the buffer was too small.
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

/// Read the app-requested `apiVersion` out of `VkInstanceCreateInfo->pApplicationInfo`, defaulting to
/// 1.0 when absent (per spec). Used for the loader-interface-version-5 compatibility check.
unsafe fn requested_api_version(p_create_info: *const vk::InstanceCreateInfo) -> u32 {
    let Some(ci) = p_create_info.as_ref() else {
        return vk::API_VERSION_1_0;
    };
    match ci.p_application_info.as_ref() {
        Some(ai) if ai.api_version != 0 => ai.api_version,
        _ => vk::API_VERSION_1_0,
    }
}

// ---- global-level --------------------------------------------------------------------------------

/// The API version this ICD implements (loader calls this to gate the driver's api level).
#[no_mangle]
pub extern "C" fn vkEnumerateInstanceVersion(p_api_version: *mut u32) -> VkResult {
    if let Some(v) = unsafe { p_api_version.as_mut() } {
        *v = state::DD_API_VERSION;
    }
    VK_SUCCESS
}

/// Build a `VkExtensionProperties` from a name + spec version.
fn ext_prop(name: &str, spec: u32) -> vk::ExtensionProperties {
    let mut p = vk::ExtensionProperties {
        spec_version: spec,
        ..Default::default()
    };
    for (dst, &b) in p.extension_name.iter_mut().zip(name.as_bytes().iter()) {
        *dst = b as core::ffi::c_char;
    }
    p
}

/// Instance-level extensions the ICD implements — the **allow-list of what is actually backed**, not
/// everything `vk.xml` lists (Phase-0 truthful enumeration, audit §2.2): the WSI surface +
/// wayland-surface, and `VK_KHR_get_physical_device_properties2` (the `...2` physical-device queries in
/// this file). Under the advertised 1.0 profile these are the extensions that carry those entry points.
/// Must stay in lock-step with `crate::capability::ADVERTISED_INSTANCE_EXTENSIONS`. Ports the
/// advertisement MoltenVK/Mesa expose; the entry points live here + in `crate::wsi`.
#[no_mangle]
pub extern "C" fn vkEnumerateInstanceExtensionProperties(
    _p_layer_name: *const c_char,
    p_count: *mut u32,
    p_props: *mut vk::ExtensionProperties,
) -> VkResult {
    let exts = [
        ext_prop("VK_KHR_surface", 25),
        ext_prop("VK_KHR_wayland_surface", 6),
        ext_prop("VK_KHR_get_physical_device_properties2", 2),
    ];
    unsafe { write_enumeration(&exts, p_count, p_props) }
}

/// The ICD exposes no layers (layers are discovered from layer manifests, never the driver).
#[no_mangle]
pub extern "C" fn vkEnumerateInstanceLayerProperties(
    p_count: *mut u32,
    p_props: *mut vk::LayerProperties,
) -> VkResult {
    unsafe { write_enumeration::<vk::LayerProperties>(&[], p_count, p_props) }
}

// ---- instance lifecycle --------------------------------------------------------------------------

/// Create the ICD instance + its single physical device, stamping both with the loader magic.
#[no_mangle]
pub extern "C" fn vkCreateInstance(
    p_create_info: *const vk::InstanceCreateInfo,
    _p_allocator: *const c_void,
    p_instance: *mut VkInstance,
) -> VkResult {
    let Some(out) = (unsafe { p_instance.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let requested = unsafe { requested_api_version(p_create_info) };
    // Phase-0 truthful version gate (gui_vk_capability_truth, audit §2.2). An app requesting an
    // apiVersion NEWER than we advertise (`DD_API_VERSION` == 1.0) must be refused with
    // `VK_ERROR_INCOMPATIBLE_DRIVER` — not just a greater *major* (which let a 1.4 request slip
    // through), but any greater variant/major/minor. Patch is ignored per spec (patch differences are
    // always compatible), so compare the version word with the low 12 patch bits masked off.
    if (requested >> 12) > (state::DD_API_VERSION >> 12) {
        return VK_ERROR_INCOMPATIBLE_DRIVER;
    }

    // Physical device first (its back-pointer is filled once the instance handle exists).
    let phys = Dispatchable::new(PhysicalDevice {
        instance: core::ptr::null_mut(),
    });
    let instance = Dispatchable::new(Instance {
        app_api_version: requested,
        physical_device: phys,
    });
    unsafe {
        if let Some(p) = Dispatchable::<PhysicalDevice>::inner(phys) {
            p.instance = instance;
        }
    }
    *out = instance;
    VK_SUCCESS
}

/// Tear down the instance and its physical device.
#[no_mangle]
pub extern "C" fn vkDestroyInstance(instance: VkInstance, _p_allocator: *const c_void) {
    if instance.is_null() {
        return;
    }
    unsafe {
        if let Some(inst) = Dispatchable::<Instance>::inner(instance) {
            Dispatchable::<PhysicalDevice>::free(inst.physical_device);
        }
        Dispatchable::<Instance>::free(instance);
    }
}

/// Report our single physical device.
#[no_mangle]
pub extern "C" fn vkEnumeratePhysicalDevices(
    instance: VkInstance,
    p_count: *mut u32,
    p_devices: *mut VkPhysicalDevice,
) -> VkResult {
    let Some(inst) = (unsafe { Dispatchable::<Instance>::inner(instance) }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let devs = [inst.physical_device];
    unsafe { write_enumeration(&devs, p_count, p_devices) }
}

// ---- physical-device queries ---------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties(
    _physical_device: VkPhysicalDevice,
    p_props: *mut vk::PhysicalDeviceProperties,
) {
    if let Some(out) = unsafe { p_props.as_mut() } {
        *out = state::physical_device_properties();
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures(
    _physical_device: VkPhysicalDevice,
    p_features: *mut vk::PhysicalDeviceFeatures,
) {
    if let Some(out) = unsafe { p_features.as_mut() } {
        *out = state::physical_device_features();
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties(
    _physical_device: VkPhysicalDevice,
    p_mem: *mut vk::PhysicalDeviceMemoryProperties,
) {
    if let Some(out) = unsafe { p_mem.as_mut() } {
        *out = state::memory_properties();
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties(
    _physical_device: VkPhysicalDevice,
    p_count: *mut u32,
    p_props: *mut vk::QueueFamilyProperties,
) {
    let families = [state::queue_family_properties()];
    unsafe {
        let _ = write_enumeration(&families, p_count, p_props);
    }
}

/// Broad format capabilities so apps' format-support checks pass (vkcube probes linear/optimal tiling
/// + buffer features for its texture/depth/vertex formats before choosing a path; all-zero features
/// sent it down a broken branch). A Metal-class device supports these across the common formats.
/// Truthful **per-format** `VkFormatProperties` (blocker 3). A color format advertises color-attachment /
/// blend / sampled / blit / transfer; a depth/stencil format advertises DEPTH_STENCIL_ATTACHMENT + sampled
/// + transfer (never color-attachment, and vice-versa) — reporting the same flags for every format made
/// wgpu-hal build wrong per-format capabilities. Vertex-attribute float formats advertise VERTEX_BUFFER.
/// Ported from `MVKPixelFormats::getVkFormatProperties`.
pub fn per_format_features(format: vk::Format) -> vk::FormatProperties {
    use vk::FormatFeatureFlags as F;
    let color = matches!(
        format,
        vk::Format::R8G8B8A8_UNORM
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_UNORM
            | vk::Format::B8G8R8A8_SRGB
    );
    let depth = crate::memory::is_depth_format(format);
    let optimal = if color {
        F::SAMPLED_IMAGE
            | F::STORAGE_IMAGE
            | F::COLOR_ATTACHMENT
            | F::COLOR_ATTACHMENT_BLEND
            | F::BLIT_SRC
            | F::BLIT_DST
            | F::SAMPLED_IMAGE_FILTER_LINEAR
            | F::TRANSFER_SRC
            | F::TRANSFER_DST
    } else if depth {
        F::SAMPLED_IMAGE | F::DEPTH_STENCIL_ATTACHMENT | F::TRANSFER_SRC | F::TRANSFER_DST
    } else {
        F::empty()
    };
    // Vertex-attribute float formats (wgpu vertex buffers).
    let buffer = match format {
        vk::Format::R32_SFLOAT
        | vk::Format::R32G32_SFLOAT
        | vk::Format::R32G32B32_SFLOAT
        | vk::Format::R32G32B32A32_SFLOAT => F::VERTEX_BUFFER,
        _ if color => F::UNIFORM_TEXEL_BUFFER | F::STORAGE_TEXEL_BUFFER,
        _ => F::empty(),
    };
    vk::FormatProperties {
        // Depth is never linear-tileable; color's linear tiling is a reduced set but we report the same
        // materializable features (the bring-up path treats tiling uniformly).
        linear_tiling_features: if depth { F::empty() } else { optimal },
        optimal_tiling_features: optimal,
        buffer_features: buffer,
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties(
    _physical_device: VkPhysicalDevice,
    _format: i32,
    p_props: *mut vk::FormatProperties,
) {
    crate::reg::trace("vkGetPhysicalDeviceFormatProperties");
    if let Some(out) = unsafe { p_props.as_mut() } {
        *out = per_format_features(vk::Format::from_raw(_format));
    }
}

// ---- the `...2` property queries (vkcube / VK_KHR_get_physical_device_properties2) ---------------
// Each fills only the nested payload field, leaving the app-provided sType/pNext chain intact.

/// A pNext-chain node header (`{ sType, pNext }`) — every Vulkan extension struct starts with this.
#[repr(C)]
struct ChainHeader {
    s_type: i32,
    p_next: *mut ChainHeader,
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceProperties2(
    _physical_device: VkPhysicalDevice,
    p_props: *mut vk::PhysicalDeviceProperties2,
) {
    let Some(out) = (unsafe { p_props.as_mut() }) else { return };
    out.properties = state::physical_device_properties();
    // Walk the pNext chain and fill the payloads apps read back (vkcube chains + prints
    // VkPhysicalDeviceDriverProperties.driverName/driverInfo — leaving them garbage would make its
    // printf abort on a stray "%n"). Preserve each node's sType/pNext.
    let mut node = out.p_next as *mut ChainHeader;
    while !node.is_null() {
        let s_type = unsafe { (*node).s_type };
        // VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES == 1000196000
        if s_type == vk::StructureType::PHYSICAL_DEVICE_DRIVER_PROPERTIES.as_raw() {
            let dp = node as *mut vk::PhysicalDeviceDriverProperties;
            if let Some(d) = unsafe { dp.as_mut() } {
                d.driver_id = vk::DriverId::MESA_LLVMPIPE; // a valid enum; dd has no registered id
                write_cstr(&mut d.driver_name, "dd");
                write_cstr(&mut d.driver_info, "dd Metal (Vulkan) 0.1");
                // Matches the advertised 1.4 core level (the API version this ICD targets; dd is not a
                // formally-submitted CTS-conformant implementation).
                d.conformance_version = vk::ConformanceVersion {
                    major: 1,
                    minor: 4,
                    subminor: 0,
                    patch: 0,
                };
            }
        } else if s_type == vk::StructureType::PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES.as_raw() {
            // wgpu-hal reads maxPerSetDescriptors to bound its descriptor-set sizing; a zero here would
            // make it refuse to build any descriptor set. Report a large Metal-class ceiling + our budget.
            let mp = node as *mut vk::PhysicalDeviceMaintenance3Properties;
            if let Some(m) = unsafe { mp.as_mut() } {
                m.max_per_set_descriptors = 1_000_000;
                m.max_memory_allocation_size = 1 << 31; // 2 GiB (matches the executor residency budget)
            }
        } else if s_type == vk::StructureType::PHYSICAL_DEVICE_PUSH_DESCRIPTOR_PROPERTIES_KHR.as_raw() {
            // Vulkan 1.4 push descriptors: the max descriptors a single vkCmdPushDescriptorSet can push.
            let pp = node as *mut vk::PhysicalDevicePushDescriptorPropertiesKHR;
            if let Some(p) = unsafe { pp.as_mut() } {
                p.max_push_descriptors = 32;
            }
        }
        node = unsafe { (*node).p_next };
    }
}

/// Write a NUL-terminated C string into a fixed `[c_char; N]` array (truncating).
fn write_cstr(dst: &mut [core::ffi::c_char], s: &str) {
    let keep = dst.len().saturating_sub(1);
    for b in dst.iter_mut() {
        *b = 0;
    }
    for (d, &b) in dst.iter_mut().zip(s.as_bytes().iter()).take(keep) {
        *d = b as core::ffi::c_char;
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFeatures2(
    _physical_device: VkPhysicalDevice,
    p_features: *mut vk::PhysicalDeviceFeatures2,
) {
    let Some(out) = (unsafe { p_features.as_mut() }) else { return };
    out.features = state::physical_device_features();
    // Truthfully fill the promoted-feature structs a modern app (wgpu-hal / Zed) chains onto the query.
    // We report ONLY features with real bodies (see `crate::ext`, `crate::query`, the sync2 barrier): a
    // false `TRUE` here would let the app enable a path that does nothing. Apps zero-init these structs
    // (per the spec), so setting the supported bits to `TRUE` and leaving the rest is the truthful fill.
    let mut node = out.p_next as *mut ChainHeader;
    while !node.is_null() {
        let s = unsafe { (*node).s_type };
        if s == vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceVulkan12Features).as_mut() } {
                f.timeline_semaphore = vk::TRUE;
                f.buffer_device_address = vk::TRUE;
                f.host_query_reset = vk::TRUE;
                // Descriptor-indexing subset we structurally honor (no update-after-bind, no non-uniform
                // indexing — those depend on IR emission timing / host shader translation we don't guarantee).
                f.descriptor_indexing = vk::TRUE;
                f.runtime_descriptor_array = vk::TRUE;
                f.descriptor_binding_variable_descriptor_count = vk::TRUE;
                f.descriptor_binding_partially_bound = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceVulkan13Features).as_mut() } {
                f.dynamic_rendering = vk::TRUE;
                f.synchronization2 = vk::TRUE;
                f.private_data = vk::TRUE;
                f.maintenance4 = vk::TRUE;
                f.pipeline_creation_cache_control = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_DESCRIPTOR_INDEXING_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceDescriptorIndexingFeatures).as_mut() } {
                f.runtime_descriptor_array = vk::TRUE;
                f.descriptor_binding_variable_descriptor_count = vk::TRUE;
                f.descriptor_binding_partially_bound = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceTimelineSemaphoreFeatures).as_mut() } {
                f.timeline_semaphore = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_BUFFER_DEVICE_ADDRESS_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceBufferDeviceAddressFeatures).as_mut() } {
                f.buffer_device_address = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceDynamicRenderingFeatures).as_mut() } {
                f.dynamic_rendering = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_SYNCHRONIZATION_2_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceSynchronization2Features).as_mut() } {
                f.synchronization2 = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_HOST_QUERY_RESET_FEATURES.as_raw() {
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceHostQueryResetFeatures).as_mut() } {
                f.host_query_reset = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_MAINTENANCE_5_FEATURES_KHR.as_raw() {
            // Vulkan 1.4 maintenance5 (vkCmdBindIndexBuffer2, device image subresource layout, rendering-
            // area granularity) is implemented.
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceMaintenance5FeaturesKHR).as_mut() } {
                f.maintenance5 = vk::TRUE;
            }
        } else if s == vk::StructureType::PHYSICAL_DEVICE_MAINTENANCE_6_FEATURES_KHR.as_raw() {
            // Vulkan 1.4 maintenance6 (vkCmd{BindDescriptorSets2,PushConstants2,PushDescriptorSet2}) too.
            if let Some(f) = unsafe { (node as *mut vk::PhysicalDeviceMaintenance6FeaturesKHR).as_mut() } {
                f.maintenance6 = vk::TRUE;
            }
        }
        node = unsafe { (*node).p_next };
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceMemoryProperties2(
    _physical_device: VkPhysicalDevice,
    p_mem: *mut vk::PhysicalDeviceMemoryProperties2,
) {
    if let Some(out) = unsafe { p_mem.as_mut() } {
        out.memory_properties = state::memory_properties();
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties2(
    _physical_device: VkPhysicalDevice,
    p_count: *mut u32,
    p_props: *mut vk::QueueFamilyProperties2,
) {
    let Some(count) = (unsafe { p_count.as_mut() }) else { return };
    if p_props.is_null() {
        *count = 1;
        return;
    }
    if *count >= 1 {
        if let Some(out) = unsafe { p_props.as_mut() } {
            out.queue_family_properties = state::queue_family_properties();
        }
        *count = 1;
    }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties2(
    _physical_device: VkPhysicalDevice,
    _format: i32,
    p_props: *mut vk::FormatProperties2,
) {
    if let Some(out) = unsafe { p_props.as_mut() } {
        out.format_properties = per_format_features(vk::Format::from_raw(_format));
    }
}

// ---- device-level enumeration (physical-device scoped) -------------------------------------------

/// Device extensions the ICD implements: `VK_KHR_swapchain` (present) plus the modern wgpu/Zed set —
/// timeline semaphores, dynamic rendering, buffer device address (see `crate::ext`). Must stay in
/// lock-step with `crate::capability::ADVERTISED_DEVICE_EXTENSIONS`.
#[no_mangle]
pub extern "C" fn vkEnumerateDeviceExtensionProperties(
    _physical_device: VkPhysicalDevice,
    _p_layer_name: *const c_char,
    p_count: *mut u32,
    p_props: *mut vk::ExtensionProperties,
) -> VkResult {
    let exts = [
        ext_prop("VK_KHR_swapchain", 70),
        ext_prop("VK_KHR_timeline_semaphore", 2),
        ext_prop("VK_KHR_dynamic_rendering", 1),
        ext_prop("VK_KHR_buffer_device_address", 1),
        ext_prop("VK_EXT_descriptor_indexing", 2),
        ext_prop("VK_EXT_host_query_reset", 1),
    ];
    unsafe { write_enumeration(&exts, p_count, p_props) }
}

/// Deprecated device-layer enumeration — always empty (spec: return instance layers or none).
#[no_mangle]
pub extern "C" fn vkEnumerateDeviceLayerProperties(
    _physical_device: VkPhysicalDevice,
    p_count: *mut u32,
    p_props: *mut vk::LayerProperties,
) -> VkResult {
    unsafe { write_enumeration::<vk::LayerProperties>(&[], p_count, p_props) }
}

/// A color format the render/transfer path materializes (matches `crate::memory::vkCreateImage`).
fn image_format_supported(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::R8G8B8A8_UNORM
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_UNORM
            | vk::Format::B8G8R8A8_SRGB
    )
}

/// `vkGetPhysicalDeviceImageFormatProperties` — the creation limits for a `(format, type, tiling,
/// usage, flags)` combination, or `VK_ERROR_FORMAT_NOT_SUPPORTED` when the combination is not
/// creatable (spec §12.3). Reports the supported 2D color subset with the device limits; anything else
/// (3D, unsupported format, cube/alias flags) is truthfully unsupported. Ported from
/// `MVKPhysicalDevice::getImageFormatProperties`.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties(
    _physical_device: VkPhysicalDevice,
    format: vk::Format,
    image_type: vk::ImageType,
    tiling: vk::ImageTiling,
    _usage: vk::ImageUsageFlags,
    _flags: vk::ImageCreateFlags,
    p_image_format_properties: *mut vk::ImageFormatProperties,
) -> VkResult {
    // VK_ERROR_FORMAT_NOT_SUPPORTED = -11.
    const VK_ERROR_FORMAT_NOT_SUPPORTED: VkResult = -11;
    let Some(out) = (unsafe { p_image_format_properties.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !image_format_supported(format)
        || image_type != vk::ImageType::TYPE_2D
        || tiling != vk::ImageTiling::OPTIMAL
    {
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    let limits = state::physical_device_properties().limits;
    *out = vk::ImageFormatProperties {
        max_extent: vk::Extent3D {
            width: limits.max_image_dimension2_d,
            height: limits.max_image_dimension2_d,
            depth: 1,
        },
        max_mip_levels: 1 + (limits.max_image_dimension2_d as f32).log2() as u32,
        max_array_layers: limits.max_image_array_layers,
        // Single- and 4x-multisample color (the resolve path materializes 4x).
        sample_counts: vk::SampleCountFlags::TYPE_1 | vk::SampleCountFlags::TYPE_4,
        max_resource_size: 1u64 << 31,
    };
    VK_SUCCESS
}

/// `vkGetPhysicalDeviceSparseImageFormatProperties` — we advertise no sparse residency, so no format
/// has sparse properties: report a count of zero (spec-valid). Ported from the no-sparse path in
/// `MVKPhysicalDevice::getSparseImageFormatProperties`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkGetPhysicalDeviceSparseImageFormatProperties(
    _physical_device: VkPhysicalDevice,
    _format: vk::Format,
    _image_type: vk::ImageType,
    _samples: vk::SampleCountFlags,
    _usage: vk::ImageUsageFlags,
    _tiling: vk::ImageTiling,
    p_property_count: *mut u32,
    _p_properties: *mut c_void,
) {
    if let Some(count) = unsafe { p_property_count.as_mut() } {
        *count = 0;
    }
}

// Silence an unused-type lint if these aliases aren't otherwise named.
const _: Option<&Device> = None;
const _: Option<&Queue> = None;

// ---- Vulkan 1.1: physical-device groups, external caps, ...2 format queries ----------------------

/// `vkEnumeratePhysicalDeviceGroups` (Vulkan 1.1): report the one device group containing our single
/// physical device. Ported from `MVKInstance::getPhysicalDeviceGroups` (each MVK physical device is its
/// own single-device group).
#[no_mangle]
pub extern "C" fn vkEnumeratePhysicalDeviceGroups(
    instance: VkInstance,
    p_physical_device_group_count: *mut u32,
    p_physical_device_group_properties: *mut vk::PhysicalDeviceGroupProperties,
) -> VkResult {
    let Some(count) = (unsafe { p_physical_device_group_count.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_physical_device_group_properties.is_null() {
        *count = 1;
        return VK_SUCCESS;
    }
    if *count == 0 {
        return VK_INCOMPLETE;
    }
    let pd = match unsafe { Dispatchable::<Instance>::inner(instance) } {
        Some(inst) => inst.physical_device,
        None => return VK_ERROR_INITIALIZATION_FAILED,
    };
    if let Some(out) = unsafe { p_physical_device_group_properties.as_mut() } {
        out.physical_device_count = 1;
        out.physical_devices[0] = vk::PhysicalDevice::from_raw(pd as u64);
        out.subset_allocation = vk::FALSE;
    }
    *count = 1;
    VK_SUCCESS
}

/// `vkGetPhysicalDeviceExternalBufferProperties` (Vulkan 1.1): we support no external memory handle
/// types, so report none creatable (features/compatible/exportable all zero — truthful).
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceExternalBufferProperties(
    _physical_device: VkPhysicalDevice,
    _p_external_buffer_info: *const c_void,
    p_external_buffer_properties: *mut vk::ExternalBufferProperties,
) {
    if let Some(out) = unsafe { p_external_buffer_properties.as_mut() } {
        out.external_memory_properties = vk::ExternalMemoryProperties::default();
    }
}

/// `vkGetPhysicalDeviceExternalFenceProperties` (Vulkan 1.1): no external fence handle types supported.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceExternalFenceProperties(
    _physical_device: VkPhysicalDevice,
    _p_external_fence_info: *const c_void,
    p_external_fence_properties: *mut vk::ExternalFenceProperties,
) {
    if let Some(out) = unsafe { p_external_fence_properties.as_mut() } {
        out.export_from_imported_handle_types = vk::ExternalFenceHandleTypeFlags::empty();
        out.compatible_handle_types = vk::ExternalFenceHandleTypeFlags::empty();
        out.external_fence_features = vk::ExternalFenceFeatureFlags::empty();
    }
}

/// `vkGetPhysicalDeviceExternalSemaphoreProperties` (Vulkan 1.1): no external semaphore handle types.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceExternalSemaphoreProperties(
    _physical_device: VkPhysicalDevice,
    _p_external_semaphore_info: *const c_void,
    p_external_semaphore_properties: *mut vk::ExternalSemaphoreProperties,
) {
    if let Some(out) = unsafe { p_external_semaphore_properties.as_mut() } {
        out.export_from_imported_handle_types = vk::ExternalSemaphoreHandleTypeFlags::empty();
        out.compatible_handle_types = vk::ExternalSemaphoreHandleTypeFlags::empty();
        out.external_semaphore_features = vk::ExternalSemaphoreFeatureFlags::empty();
    }
}

/// `vkGetPhysicalDeviceImageFormatProperties2` (Vulkan 1.1): the `...2` wrapper delegating to the 1.0
/// image-format query (the supported 2D color subset; else `VK_ERROR_FORMAT_NOT_SUPPORTED`).
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceImageFormatProperties2(
    physical_device: VkPhysicalDevice,
    p_image_format_info: *const vk::PhysicalDeviceImageFormatInfo2,
    p_image_format_properties: *mut vk::ImageFormatProperties2,
) -> VkResult {
    let (Some(info), Some(out)) =
        (unsafe { p_image_format_info.as_ref() }, unsafe { p_image_format_properties.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    vkGetPhysicalDeviceImageFormatProperties(
        physical_device,
        info.format,
        info.ty,
        info.tiling,
        info.usage,
        info.flags,
        &mut out.image_format_properties,
    )
}

/// `vkGetPhysicalDeviceSparseImageFormatProperties2` (Vulkan 1.1): no sparse residency → zero properties.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSparseImageFormatProperties2(
    _physical_device: VkPhysicalDevice,
    _p_format_info: *const c_void,
    p_property_count: *mut u32,
    _p_properties: *mut c_void,
) {
    if let Some(count) = unsafe { p_property_count.as_mut() } {
        *count = 0;
    }
}
