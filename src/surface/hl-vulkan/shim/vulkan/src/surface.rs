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

use crate::state::StateStore;
use crate::types::*;

// ---- platform surface constructors (all mint the same modeled surface) ---------------------------

/// Mint a modeled `VkSurfaceKHR`, writing it to `p_surface`. Every platform constructor funnels here.
struct Surface;
impl Surface {
    fn create(p_surface: *mut u64) -> VkResult {
        if p_surface.is_null() {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        let handle = StateStore::with(|s| s.mint_surface());
        unsafe { *p_surface = handle };
        VK_SUCCESS
    }
}

pub extern "C" fn vkCreateXcbSurfaceKHR(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    Surface::create(p_surface)
}

pub extern "C" fn vkCreateXlibSurfaceKHR(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    Surface::create(p_surface)
}

pub extern "C" fn vkCreateWaylandSurfaceKHR(
    _instance: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    if p_surface.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    // Capture the app's OWN wayland handles (its `wl_display*` + `wl_surface*`, on its own
    // `libwayland-client` connection) so `vkQueuePresentKHR` can marshal the presented frame onto that
    // exact `wl_surface`. The pointers are recorded (as raw addresses), never dereferenced here.
    let Some(ci) = (unsafe { (p_create_info as *const VkWaylandSurfaceCreateInfoKHR).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let window = crate::state::WaylandWindow {
        display: ci.display as usize,
        surface: ci.surface as usize,
    };
    let handle = StateStore::with(|s| {
        let h = s.mint_surface();
        s.wayland_surfaces.insert(h, window);
        h
    });
    unsafe { *p_surface = handle };
    VK_SUCCESS
}

pub extern "C" fn vkCreateHeadlessSurfaceEXT(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    Surface::create(p_surface)
}

pub extern "C" fn vkDestroySurfaceKHR(
    _instance: *mut c_void,
    surface: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.surfaces.remove(&surface);
        s.wayland_surfaces.remove(&surface);
    });
}

// ---- physical-device presentation-support queries (the lone family presents) ---------------------

pub extern "C" fn vkGetPhysicalDeviceXcbPresentationSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    _connection: *mut c_void,
    _visual_id: u32,
) -> VkBool32 {
    PresentationSupport::query(queue_family_index)
}

pub extern "C" fn vkGetPhysicalDeviceXlibPresentationSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    _dpy: *mut c_void,
    _visual_id: u64,
) -> VkBool32 {
    PresentationSupport::query(queue_family_index)
}

pub extern "C" fn vkGetPhysicalDeviceWaylandPresentationSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    _display: *mut c_void,
) -> VkBool32 {
    PresentationSupport::query(queue_family_index)
}

struct PresentationSupport;
impl PresentationSupport {
    fn query(queue_family_index: u32) -> VkBool32 {
        if present::QueueFamily(queue_family_index).supports_present() {
            VK_TRUE
        } else {
            VK_FALSE
        }
    }
}

// ---- physical-device surface queries -------------------------------------------------------------

pub extern "C" fn vkGetPhysicalDeviceSurfaceSupportKHR(
    _physical_device: *mut c_void,
    queue_family_index: u32,
    surface: u64,
    p_supported: *mut VkBool32,
) -> VkResult {
    let Some(out) = (unsafe { p_supported.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !StateStore::with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    *out = if present::QueueFamily(queue_family_index).supports_present() {
        VK_TRUE
    } else {
        VK_FALSE
    };
    VK_SUCCESS
}

pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
    _physical_device: *mut c_void,
    surface: u64,
    p_surface_capabilities: *mut VkSurfaceCapabilitiesKHR,
) -> VkResult {
    let Some(out) = (unsafe { p_surface_capabilities.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !StateStore::with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let c = present::surface_capabilities();
    *out = VkSurfaceCapabilitiesKHR {
        min_image_count: c.min_image_count,
        max_image_count: c.max_image_count,
        current_extent: VkExtent2D {
            width: c.current_extent.0,
            height: c.current_extent.1,
        },
        min_image_extent: VkExtent2D {
            width: c.min_image_extent.0,
            height: c.min_image_extent.1,
        },
        max_image_extent: VkExtent2D {
            width: c.max_image_extent.0,
            height: c.max_image_extent.1,
        },
        max_image_array_layers: c.max_image_array_layers,
        supported_transforms: c.supported_transforms,
        current_transform: c.current_transform,
        supported_composite_alpha: c.supported_composite_alpha,
        supported_usage_flags: c.supported_usage_flags,
    };
    VK_SUCCESS
}

pub extern "C" fn vkGetPhysicalDeviceSurfaceFormatsKHR(
    _physical_device: *mut c_void,
    surface: u64,
    p_surface_format_count: *mut u32,
    p_surface_formats: *mut VkSurfaceFormatKHR,
) -> VkResult {
    if !StateStore::with(|s| s.surface_valid(surface)) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let formats: Vec<VkSurfaceFormatKHR> = present::surface_formats()
        .into_iter()
        .map(|f| VkSurfaceFormatKHR {
            format: f.format as i32,
            color_space: f.color_space,
        })
        .collect();
    unsafe { write_enumeration(&formats, p_surface_format_count, p_surface_formats) }
}

pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModesKHR(
    _physical_device: *mut c_void,
    surface: u64,
    p_present_mode_count: *mut u32,
    p_present_modes: *mut i32,
) -> VkResult {
    if !StateStore::with(|s| s.surface_valid(surface)) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateStore;

    /// `vkCreateWaylandSurfaceKHR` records the app's OWN `wl_display*` + `wl_surface*` (from
    /// `VkWaylandSurfaceCreateInfoKHR`) against the minted `VkSurfaceKHR`, so a later present can marshal
    /// onto that exact surface. This is the Vulkan side of the "capture at surface creation" milestone.
    #[test]
    fn wayland_surface_creation_captures_display_and_surface() {
        // Fake, never-dereferenced native handles standing in for the app's libwayland objects.
        let fake_display = 0xD15_9000usize as *mut c_void;
        let fake_surface = 0x5_FACE_00usize as *mut c_void;
        let ci = VkWaylandSurfaceCreateInfoKHR {
            s_type: 0,
            p_next: core::ptr::null(),
            flags: 0,
            display: fake_display,
            surface: fake_surface,
        };
        let mut handle: u64 = 0;
        assert_eq!(
            vkCreateWaylandSurfaceKHR(
                core::ptr::null_mut(),
                &ci as *const _ as *const c_void,
                core::ptr::null(),
                &mut handle,
            ),
            VK_SUCCESS
        );
        assert_ne!(handle, 0, "a live VkSurfaceKHR must be minted");
        // The captured pointers are stored (as raw addresses) keyed by the surface handle.
        let win = StateStore::with(|s| s.wayland_surfaces.get(&handle).copied())
            .expect("wayland window captured");
        assert_eq!(win.display, fake_display as usize);
        assert_eq!(win.surface, fake_surface as usize);
        assert!(
            StateStore::with(|s| s.surface_valid(handle)),
            "surface handle must be live"
        );

        // Destroy clears both the surface set and the captured window.
        vkDestroySurfaceKHR(core::ptr::null_mut(), handle, core::ptr::null());
        assert!(StateStore::with(|s| s
            .wayland_surfaces
            .get(&handle)
            .is_none()));
        assert!(!StateStore::with(|s| s.surface_valid(handle)));
    }

    /// A null `pCreateInfo` is rejected (no faked capture), and a null `pSurface` too.
    #[test]
    fn wayland_surface_creation_rejects_null_pointers() {
        let mut handle: u64 = 0;
        assert_eq!(
            vkCreateWaylandSurfaceKHR(
                core::ptr::null_mut(),
                core::ptr::null(),
                core::ptr::null(),
                &mut handle
            ),
            VK_ERROR_INITIALIZATION_FAILED
        );
        let ci = VkWaylandSurfaceCreateInfoKHR {
            s_type: 0,
            p_next: core::ptr::null(),
            flags: 0,
            display: core::ptr::null_mut(),
            surface: core::ptr::null_mut(),
        };
        assert_eq!(
            vkCreateWaylandSurfaceKHR(
                core::ptr::null_mut(),
                &ci as *const _ as *const c_void,
                core::ptr::null(),
                core::ptr::null_mut(),
            ),
            VK_ERROR_INITIALIZATION_FAILED
        );
    }

    /// `vkGetPhysicalDeviceWaylandPresentationSupportKHR` reports the lone present family as supported.
    #[test]
    fn wayland_presentation_support_is_true_for_the_present_family() {
        assert_eq!(
            vkGetPhysicalDeviceWaylandPresentationSupportKHR(
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut()
            ),
            VK_TRUE
        );
        // A non-present family index is not supported.
        assert_eq!(
            vkGetPhysicalDeviceWaylandPresentationSupportKHR(
                core::ptr::null_mut(),
                7,
                core::ptr::null_mut()
            ),
            VK_FALSE
        );
    }
}
