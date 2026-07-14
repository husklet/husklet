//! The WSI surface path: `VK_KHR_surface` + the platform `vkCreate*SurfaceKHR` constructors and the
//! physical-device surface queries a swapchain-building app runs before it creates a device.
//!
//! A `VkSurfaceKHR` is an INSTANCE-level object (created before any logical device), so its handle is
//! minted + tracked in the process-global [`crate::state`], NOT in the `hl_vulkan::Device`. Every
//! platform constructor (Xcb/Xlib/Wayland/headless) mints the same modeled surface — the guest driver
//! presents through the compositor regardless of the native window system, so the platform-specific
//! handles are validated (non-null) and otherwise not interpreted. The four
//! `vkGetPhysicalDeviceSurface*KHR` queries report the modeled capabilities/formats/present-modes from
//! the `hl_vulkan::service::present` surface model (a single authoritative, unit-tested source), erroring
//! `VK_ERROR_SURFACE_LOST_KHR` on an unknown/destroyed surface.

use core::ffi::c_void;

use hl_vulkan::service::present;

use crate::state::with;
use crate::types::*;

// ---- platform surface constructors (all mint the same modeled surface) ---------------------------

/// Mint a modeled `VkSurfaceKHR`, writing it to `p_surface`. Every platform constructor funnels here.
fn create_surface(p_surface: *mut u64) -> VkResult {
    if p_surface.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let handle = with(|s| s.mint_surface());
    unsafe { *p_surface = handle };
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkCreateXcbSurfaceKHR(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    create_surface(p_surface)
}

#[no_mangle]
pub extern "C" fn vkCreateXlibSurfaceKHR(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    create_surface(p_surface)
}

#[no_mangle]
pub extern "C" fn vkCreateWaylandSurfaceKHR(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    create_surface(p_surface)
}

#[no_mangle]
pub extern "C" fn vkCreateHeadlessSurfaceEXT(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    create_surface(p_surface)
}

#[no_mangle]
pub extern "C" fn vkDestroySurfaceKHR(_instance: *mut c_void, surface: u64, _p_allocator: *const c_void) {
    with(|s| {
        s.surfaces.remove(&surface);
    });
}

// ---- physical-device presentation-support queries (the lone family presents) ---------------------

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceXcbPresentationSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    _connection: *mut c_void,
    _visual_id: u32,
) -> VkBool32 {
    present_support(queue_family_index)
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceXlibPresentationSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    _dpy: *mut c_void,
    _visual_id: u64,
) -> VkBool32 {
    present_support(queue_family_index)
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceWaylandPresentationSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    _display: *mut c_void,
) -> VkBool32 {
    present_support(queue_family_index)
}

fn present_support(queue_family_index: u32) -> VkBool32 {
    if present::surface_supports_present(queue_family_index) {
        VK_TRUE
    } else {
        VK_FALSE
    }
}

// ---- physical-device surface queries -------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    surface: u64,
    p_supported: *mut VkBool32,
) -> VkResult {
    let Some(out) = (unsafe { p_supported.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    *out = if present::surface_supports_present(queue_family_index) { VK_TRUE } else { VK_FALSE };
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
    _physical_device: *mut c_void,
    surface: u64,
    p_surface_capabilities: *mut VkSurfaceCapabilitiesKHR,
) -> VkResult {
    let Some(out) = (unsafe { p_surface_capabilities.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let c = present::surface_capabilities();
    *out = VkSurfaceCapabilitiesKHR {
        min_image_count: c.min_image_count,
        max_image_count: c.max_image_count,
        current_extent: VkExtent2D { width: c.current_extent.0, height: c.current_extent.1 },
        min_image_extent: VkExtent2D { width: c.min_image_extent.0, height: c.min_image_extent.1 },
        max_image_extent: VkExtent2D { width: c.max_image_extent.0, height: c.max_image_extent.1 },
        max_image_array_layers: c.max_image_array_layers,
        supported_transforms: c.supported_transforms,
        current_transform: c.current_transform,
        supported_composite_alpha: c.supported_composite_alpha,
        supported_usage_flags: c.supported_usage_flags,
    };
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceFormatsKHR(
    _physical_device: *mut c_void,
    surface: u64,
    p_surface_format_count: *mut u32,
    p_surface_formats: *mut VkSurfaceFormatKHR,
) -> VkResult {
    if !with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let formats: Vec<VkSurfaceFormatKHR> = present::surface_formats()
        .into_iter()
        .map(|f| VkSurfaceFormatKHR { format: f.format as i32, color_space: f.color_space })
        .collect();
    unsafe { write_enumeration(&formats, p_surface_format_count, p_surface_formats) }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModesKHR(
    _physical_device: *mut c_void,
    surface: u64,
    p_present_mode_count: *mut u32,
    p_present_modes: *mut i32,
) -> VkResult {
    if !with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let modes = present::surface_present_modes();
    unsafe { write_enumeration(&modes, p_present_mode_count, p_present_modes) }
}

/// The `count`/`data` two-call enumeration pattern (`p_data` NULL = size query; else fill min(cap, len)
/// and report `VK_INCOMPLETE` on truncation). Mirrors the instance-layer `write_enumeration`.
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
