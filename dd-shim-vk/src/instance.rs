//! Instance-level + physical-device entry points (real bodies).
//!
//! Ported from MoltenVK's `MVKInstance` / `MVKPhysicalDevice` object model and the Vulkan spec
//! semantics; the property values come from [`crate::state`] (the "dd Metal (Vulkan)" device). The
//! big out-structs (`VkPhysicalDeviceProperties`, …) are `ash::vk` types so the ABI is spec-exact.

use crate::handle::Dispatchable;
use crate::state::{self, Device, Instance, PhysicalDevice, Queue};
use crate::types::*;
use ash::vk;
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

/// Instance-level extensions the ICD implements: the WSI surface + wayland-surface (so a windowed
/// Vulkan app / the loader discover and enable them). Ports the `VK_KHR_surface`/`VK_KHR_wayland_surface`
/// advertisement MoltenVK/Mesa expose; the entry points live in `crate::wsi`.
#[no_mangle]
pub extern "C" fn vkEnumerateInstanceExtensionProperties(
    _p_layer_name: *const c_char,
    p_count: *mut u32,
    p_props: *mut vk::ExtensionProperties,
) -> VkResult {
    let exts = [
        ext_prop("VK_KHR_surface", 25),
        ext_prop("VK_KHR_wayland_surface", 6),
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
    // Loader-interface v5: the loader validates apiVersion, but we still refuse a variant/major we
    // cannot honor (defensive; matches the spec table in LoaderDriverInterface.md).
    if vk::api_version_major(requested) > 1 {
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

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFormatProperties(
    _physical_device: VkPhysicalDevice,
    _format: i32,
    p_props: *mut vk::FormatProperties,
) {
    // Foundation: report no format capabilities yet (all-zero). Refined against Metal's pixel-format
    // table (MVKPixelFormats) in a later increment.
    if let Some(out) = unsafe { p_props.as_mut() } {
        *out = vk::FormatProperties::default();
    }
}

// ---- device-level enumeration (physical-device scoped) -------------------------------------------

/// Device extensions the ICD implements: `VK_KHR_swapchain` (present), so a windowed app finds it.
#[no_mangle]
pub extern "C" fn vkEnumerateDeviceExtensionProperties(
    _physical_device: VkPhysicalDevice,
    _p_layer_name: *const c_char,
    p_count: *mut u32,
    p_props: *mut vk::ExtensionProperties,
) -> VkResult {
    let exts = [ext_prop("VK_KHR_swapchain", 70)];
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

// Silence an unused-type lint if these aliases aren't otherwise named.
const _: Option<&Device> = None;
const _: Option<&Queue> = None;
