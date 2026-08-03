//! Guest cdylib deployed as `libvk_hl.so.1` — the drop-in Vulkan ICD.
//!
//! The Vulkan loader loads this via `icd.json` (`library_path: ./libvk_hl.so`), negotiates the ICD
//! interface through the hand-written `vk_icd*` hooks ([`icd`]), and resolves the whole `vk*` command
//! surface by name through [`dispatch_addr`]. That surface is code-generated from
//! `registry/vk_commands.manifest` (`build.rs`) so it can never drift from the golden 712-command set;
//! the bring-up + compute core ([`instance`]/[`device`]/[`compute`]) have real bodies that marshal the
//! C ABI and call the `hl_vulkan` lowering services through a process-global
//! [`hl_gpu::RemoteCommandSink`] over `$HL_GPU_EXEC` ([`state`]); the long tail are benign, truthful,
//! correct-ABI default stubs ([`stub`]) ported to real bodies incrementally without ever changing the
//! surface.
//!
//! The soname `libvk_hl.so.1` is baked by `build.rs` (Linux); the DT_SONAME is what the loader loads.
//!
//! EXPORTED SURFACE: only the three `vk_icd*` hooks carry `#[no_mangle]`. The `vk*` commands are
//! deliberately NOT dynamically exported — the loader reaches them through `vk_icdGetInstanceProcAddr`
//! -> [`dispatch_addr`], which resolves link-time addresses inside this object. Exporting them made the
//! driver's definitions preempt the LOADER's own inside a process where both are in global scope; the
//! loader's recursion guard then fired ("vkEnumerateInstanceExtensionProperties points to the loader")
//! and DISCARDED this driver's instance-extension list, so `vkCreateInstance` failed with
//! `VK_ERROR_EXTENSION_NOT_PRESENT` for extensions the driver does advertise. `-Wl,-Bsymbolic` cannot
//! prevent that: it binds only our own references, not our definitions' visibility to others.

// The generated + hand-written entry-point surface uses the Vulkan C names verbatim (vkCreateBuffer, …).
#![allow(non_snake_case)]

pub mod address;
pub mod compute;
pub mod corebits;
pub mod debug;
pub mod devgroup;
pub mod device;
pub mod dynstate;
mod feature_structs;
pub mod graphics;
pub mod hostcopy;
pub mod icd;
pub mod instance;
pub mod logging;
pub mod maintenance;
pub mod promoted_features;
pub mod state;
pub mod stub;
pub mod surface;
pub mod sync;
pub mod transfer;
pub mod types;
pub mod unsupported;

// Bring every hand-written entry point into crate-root scope so the generated
// `dispatch_addr` resolver (which references each command by its bare name) resolves them uniformly
// alongside the generated stubs.
#[allow(unused_imports)]
use address::*;
#[allow(unused_imports)]
use compute::*;
#[allow(unused_imports)]
use corebits::*;
#[allow(unused_imports)]
use debug::*;
#[allow(unused_imports)]
use devgroup::*;
#[allow(unused_imports)]
use device::*;
#[allow(unused_imports)]
use dynstate::*;
#[allow(unused_imports)]
use graphics::*;
#[allow(unused_imports)]
use hostcopy::*;
#[allow(unused_imports)]
use icd::*;
#[allow(unused_imports)]
use instance::*;
#[allow(unused_imports)]
use maintenance::*;
#[allow(unused_imports)]
use surface::*;
#[allow(unused_imports)]
use sync::*;
#[allow(unused_imports)]
use transfer::*;
#[allow(unused_imports)]
use unsupported::*;

// The generated C-ABI export surface: every `vk*` command not hand-written above (as a default stub),
// plus the `dispatch_addr` / `DISPATCH_NAMES` census the loader-facing resolvers consult.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported `vk*` entry points (hand-written + generated) — the completeness census (excludes the
/// 3 hand-written `vk_icd*` loader hooks, which are not Vulkan API commands).
pub const TOTAL_ENTRYPOINTS: usize = VK_ENTRYPOINTS;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod queue_tests;
