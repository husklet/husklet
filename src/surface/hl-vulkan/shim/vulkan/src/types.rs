//! Vulkan C-ABI types used by the hand-written ICD entry points.
//!
//! The public surface remains flat through re-exports while cohesive implementation modules group
//! loader primitives, physical-device queries, creation inputs, graphics, resources, transfers,
//! synchronization, device support, and submission structures.

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
pub const VK_ERROR_EXTENSION_NOT_PRESENT: VkResult = -7;
pub const VK_ERROR_FEATURE_NOT_PRESENT: VkResult = -8;
pub const VK_ERROR_INCOMPATIBLE_DRIVER: VkResult = -9;
pub const VK_ERROR_UNKNOWN: VkResult = -13;
pub const VK_ERROR_INVALID_EXTERNAL_HANDLE: VkResult = -1_000_072_003;
/// `VK_ERROR_SURFACE_LOST_KHR` (`VK_KHR_surface`, stable ABI) — an unknown/destroyed surface.
pub const VK_ERROR_SURFACE_LOST_KHR: VkResult = -1_000_000_000;
/// `VK_ERROR_NATIVE_WINDOW_IN_USE_KHR` — a second surface over a window already claimed by one.
pub const VK_ERROR_NATIVE_WINDOW_IN_USE_KHR: VkResult = -1_000_000_001;

/// The Vulkan API version this ICD advertises: **Vulkan 1.3.0** (mirrors `hl_vulkan::result`, which
/// carries the full rationale for why 1.3 and not 1.4).
pub const HL_API_VERSION: u32 = make_api_version(0, 1, 3, 0);
pub const HL_DRIVER_VERSION: u32 = make_api_version(0, 0, 1, 0);

/// `VK_MAKE_API_VERSION(variant, major, minor, patch)` — the stable Vulkan version packing.
pub const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
}

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
        Box::into_raw(Box::new(Dispatchable {
            loader_data: ICD_LOADER_MAGIC,
            inner,
        })) as *mut c_void
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

mod creation;
mod device_support;
mod graphics;
mod physical_device;
mod resources;
mod submission;
mod synchronization;
mod transfer;

pub use creation::*;
pub use device_support::*;
pub use graphics::*;
pub use physical_device::*;
pub use resources::*;
pub use submission::*;
pub use synchronization::*;
pub use transfer::*;
