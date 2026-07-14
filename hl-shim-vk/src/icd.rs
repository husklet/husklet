//! The loader ↔ ICD interface — the private protocol the Vulkan **loader** uses to discover and
//! accept this driver. Ported directly from the authoritative Khronos sources:
//!   * Vulkan-Loader `docs/LoaderDriverInterface.md` (negotiation, entry-point discovery, the
//!     version-5 API-version compatibility rule that is the usual `VK_ERROR_INCOMPATIBLE_DRIVER`
//!     root cause), and `include/vulkan/vk_icd.h` (interface version constants).
//!   * MoltenVK `MoltenVK/Vulkan/vulkan.mm` (`vk_icdNegotiateLoaderICDInterfaceVersion`,
//!     `vk_icdGetInstanceProcAddr`, `vk_icdGetPhysicalDeviceProcAddr`) — mirrored 1:1 in Rust.
//!
//! ## Why the prior attempt got `VK_ERROR_INCOMPATIBLE_DRIVER`, and the fix
//! The loader rejects a driver (its physical devices never appear) if ANY of: it can't find
//! `vk_icdGetInstanceProcAddr`; negotiation returns `VK_ERROR_INCOMPATIBLE_DRIVER` or a version the
//! loader dropped; or a dispatchable object it hands back lacks the loader-magic slot (see
//! `crate::handle`). We satisfy all three: export `vk_icdNegotiateLoaderICDInterfaceVersion`
//! (agree on interface version ≤ 5, exactly like MoltenVK), export `vk_icdGetInstanceProcAddr`
//! resolving the whole generated `vk*` surface, and stamp every dispatchable object with the magic.

use crate::types::*;
use core::ffi::{c_char, c_void};

/// `PFN_vkVoidFunction` — the type both proc-addr resolvers return.
pub type PFN_vkVoidFunction = Option<unsafe extern "C" fn()>;

/// The loader ↔ ICD interface version we support. MoltenVK negotiates 5; 5 is the version at which
/// the loader (not the driver) validates the requested API version, so a modern loader accepts us.
const HL_ICD_INTERFACE_VERSION: u32 = 5;

/// Resolve a `vk*` name to its address via the generated [`crate::dispatch_addr`] resolver.
fn resolve(name: &str) -> PFN_vkVoidFunction {
    // The address is a real `#[no_mangle] extern "C"` entry point; re-type it as the opaque PFN the
    // loader expects (all fn pointers share one representation on the guest targets).
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

/// Loader ↔ ICD interface-version negotiation. Ported from MoltenVK `vk_icdNegotiateLoaderICD…`:
/// agree on `min(loader, ours)`; only report `VK_ERROR_INCOMPATIBLE_DRIVER` if the loader is older
/// than our minimum (interface 5). `pSupportedVersion` is in/out (loader's max in, agreed out).
#[no_mangle]
pub extern "C" fn vk_icdNegotiateLoaderICDInterfaceVersion(p_supported_version: *mut u32) -> VkResult {
    let Some(ver) = (unsafe { p_supported_version.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if *ver >= HL_ICD_INTERFACE_VERSION {
        *ver = HL_ICD_INTERFACE_VERSION;
        VK_SUCCESS
    } else {
        // Loader too old to guarantee the version-5 contract we rely on.
        VK_ERROR_INCOMPATIBLE_DRIVER
    }
}

/// The loader's primary entry-point discovery hook (`docs/LoaderDriverInterface.md` §"Driver Vulkan
/// Entry Point Discovery"). Global-level names resolve with a NULL instance; instance/device-level
/// names resolve with a non-NULL instance. We special-case the two ICD hooks (as MoltenVK does),
/// then defer to `vkGetInstanceProcAddr` for the whole `vk*` surface.
#[no_mangle]
pub extern "C" fn vk_icdGetInstanceProcAddr(instance: VkInstance, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    match name {
        "vk_icdNegotiateLoaderICDInterfaceVersion" => {
            Some(unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(vk_icdNegotiateLoaderICDInterfaceVersion as *const () as usize) })
        }
        "vk_icdGetPhysicalDeviceProcAddr" => {
            Some(unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(vk_icdGetPhysicalDeviceProcAddr as *const () as usize) })
        }
        _ => vkGetInstanceProcAddr(instance, p_name),
    }
}

/// Interface version 4+: lets the loader distinguish physical-device entry points from device ones
/// (`docs/LoaderDriverInterface.md` §"Driver Unknown Physical Device Extensions"). Ported from
/// MoltenVK: return a pointer ONLY for names whose primary dispatch handle is `VkPhysicalDevice`
/// (all our `vkGetPhysicalDevice*` do), else NULL so the loader treats it as a device function.
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

/// The public `vkGetInstanceProcAddr`. Resolves the whole generated `vk*` surface by name. Returns
/// itself for `"vkGetInstanceProcAddr"` (the loader bootstraps through this). Global-vs-instance
/// filtering is left permissive — the loader enforces the NULL-instance global rule on its side.
#[no_mangle]
pub extern "C" fn vkGetInstanceProcAddr(_instance: VkInstance, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    if name == "vkGetInstanceProcAddr" {
        return Some(unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(vkGetInstanceProcAddr as *const () as usize) });
    }
    resolve(name)
}

/// The public `vkGetDeviceProcAddr`. Resolves device-level entry points by name from the same table.
#[no_mangle]
pub extern "C" fn vkGetDeviceProcAddr(_device: VkDevice, p_name: *const c_char) -> PFN_vkVoidFunction {
    let name = cstr(p_name);
    if name == "vkGetDeviceProcAddr" {
        return Some(unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(vkGetDeviceProcAddr as *const () as usize) });
    }
    resolve(name)
}

// Keep an unused import from tripping the linter when c_void isn't otherwise referenced here.
const _: *const c_void = core::ptr::null();
