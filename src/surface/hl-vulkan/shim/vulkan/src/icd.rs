//! The loader ↔ ICD interface — the private protocol the Vulkan **loader** uses to discover and accept
//! this driver. Ported 1:1 from `hl-shim-vk/src/icd.rs` (itself from Vulkan-Loader
//! `docs/LoaderDriverInterface.md` + `vk_icd.h`, and MoltenVK `vulkan.mm`).
//!
//! The loader rejects a driver (its physical devices never appear) unless: it finds
//! `vk_icdGetInstanceProcAddr`; negotiation agrees on interface version ≤ 5; and the entry-point
//! resolver returns real addresses. We export `vk_icdNegotiateLoaderICDInterfaceVersion` (agree on
//! ≤ 5, like MoltenVK), `vk_icdGetInstanceProcAddr` resolving the whole generated `vk*` surface via
//! [`crate::dispatch_addr`], and `vk_icdGetPhysicalDeviceProcAddr` for the physical-device tail.

use crate::types::*;
use core::ffi::{c_char, c_void};

/// `PFN_vkVoidFunction` — the type both proc-addr resolvers return.
#[allow(non_camel_case_types)]
pub type PFN_vkVoidFunction = Option<unsafe extern "C" fn()>;

/// The loader ↔ ICD interface version we support. MoltenVK negotiates 5; 5 is the version at which the
/// loader (not the driver) validates the requested API version, so a modern loader accepts us.
const HL_ICD_INTERFACE_VERSION: u32 = 5;

/// Resolve a `vk*` name to its address via the generated [`crate::dispatch_addr`] resolver.
fn resolve(name: &str) -> PFN_vkVoidFunction {
    crate::dispatch_addr(name)
        .map(|addr| unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(addr) })
}

/// Borrow a `*const c_char` as `&str` (empty on NULL / bad UTF-8).
fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    unsafe { core::ffi::CStr::from_ptr(p) }.to_str().unwrap_or("")
}

// ---- ICD entry points (NOT Vulkan API commands; a private loader<->driver protocol) --------------

/// Loader ↔ ICD interface-version negotiation. Agree on `min(loader, ours)`; report
/// `VK_ERROR_INCOMPATIBLE_DRIVER` only if the loader is older than our minimum (interface 5).
#[no_mangle]
pub extern "C" fn vk_icdNegotiateLoaderICDInterfaceVersion(p_supported_version: *mut u32) -> VkResult {
    let Some(ver) = (unsafe { p_supported_version.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if *ver >= HL_ICD_INTERFACE_VERSION {
        *ver = HL_ICD_INTERFACE_VERSION;
        VK_SUCCESS
    } else {
        VK_ERROR_INCOMPATIBLE_DRIVER
    }
}

/// The loader's primary entry-point discovery hook. We special-case the two ICD hooks (as MoltenVK
/// does), then defer to `vkGetInstanceProcAddr` for the whole `vk*` surface.
#[no_mangle]
pub extern "C" fn vk_icdGetInstanceProcAddr(instance: VkInstance, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    match name {
        "vk_icdNegotiateLoaderICDInterfaceVersion" => Some(unsafe {
            core::mem::transmute::<usize, unsafe extern "C" fn()>(
                vk_icdNegotiateLoaderICDInterfaceVersion as *const () as usize,
            )
        }),
        "vk_icdGetPhysicalDeviceProcAddr" => Some(unsafe {
            core::mem::transmute::<usize, unsafe extern "C" fn()>(
                vk_icdGetPhysicalDeviceProcAddr as *const () as usize,
            )
        }),
        _ => vkGetInstanceProcAddr(instance, p_name),
    }
}

/// Interface version 4+: lets the loader distinguish physical-device entry points from device ones.
/// Return a pointer ONLY for names whose primary dispatch handle is `VkPhysicalDevice`.
#[no_mangle]
pub extern "C" fn vk_icdGetPhysicalDeviceProcAddr(_instance: VkInstance, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    if name.starts_with("vkGetPhysicalDevice") || name == "vkEnumerateDeviceExtensionProperties" {
        resolve(name)
    } else {
        None
    }
}

// ---- vkGetInstanceProcAddr / vkGetDeviceProcAddr (real Vulkan API commands) -----------------------

/// The public `vkGetInstanceProcAddr`. Resolves the whole generated `vk*` surface by name; returns
/// itself for `"vkGetInstanceProcAddr"` (the loader bootstraps through this).
#[no_mangle]
pub extern "C" fn vkGetInstanceProcAddr(_instance: VkInstance, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    if name == "vkGetInstanceProcAddr" {
        return Some(unsafe {
            core::mem::transmute::<usize, unsafe extern "C" fn()>(vkGetInstanceProcAddr as *const () as usize)
        });
    }
    resolve(name)
}

/// The public `vkGetDeviceProcAddr`. Resolves device-level entry points by name from the same table.
#[no_mangle]
pub extern "C" fn vkGetDeviceProcAddr(_device: VkDevice, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    if name == "vkGetDeviceProcAddr" {
        return Some(unsafe {
            core::mem::transmute::<usize, unsafe extern "C" fn()>(vkGetDeviceProcAddr as *const () as usize)
        });
    }
    resolve(name)
}

// Keep an unused import from tripping the linter when c_void isn't otherwise referenced here.
const _: *const c_void = core::ptr::null();
