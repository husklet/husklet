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

// The generated + hand-written entry-point surface uses the Vulkan C names verbatim (vkCreateBuffer, …).
#![allow(non_snake_case)]

pub mod compute;
pub mod device;
pub mod graphics;
pub mod icd;
pub mod instance;
pub mod state;
pub mod stub;
pub mod sync;
pub mod transfer;
pub mod types;

// Bring every hand-written `#[no_mangle]` entry point into crate-root scope so the generated
// `dispatch_addr` resolver (which references each command by its bare name) resolves them uniformly
// alongside the generated stubs.
#[allow(unused_imports)]
use compute::*;
#[allow(unused_imports)]
use device::*;
#[allow(unused_imports)]
use graphics::*;
#[allow(unused_imports)]
use icd::*;
#[allow(unused_imports)]
use instance::*;
#[allow(unused_imports)]
use sync::*;
#[allow(unused_imports)]
use transfer::*;

// The generated C-ABI export surface: every `vk*` command not hand-written above (as a default stub),
// plus the `dispatch_addr` / `DISPATCH_NAMES` census the loader-facing resolvers consult.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported `vk*` entry points (hand-written + generated) — the completeness census (excludes the
/// 3 hand-written `vk_icd*` loader hooks, which are not Vulkan API commands).
pub const TOTAL_ENTRYPOINTS: usize = VK_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(VK_ENTRYPOINTS, 712, "Vulkan command surface drifted from the golden 712");
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
        assert_eq!(DISPATCH_NAMES.len(), 712, "dispatch census drifted");
    }

    #[test]
    fn every_implemented_command_resolves() {
        // Spot-check that the hand-written bring-up + compute commands resolve through the same
        // name→address table the loader uses.
        for name in [
            "vkGetInstanceProcAddr",
            "vkCreateInstance",
            "vkEnumeratePhysicalDevices",
            "vkGetPhysicalDeviceProperties",
            "vkCreateDevice",
            "vkCreateBuffer",
            "vkCreateShaderModule",
            "vkCreateComputePipelines",
            "vkQueueSubmit",
            "vkWaitForFences",
        ] {
            assert!(dispatch_addr(name).is_some(), "{name} does not resolve");
        }
    }
}
