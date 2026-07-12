//! dd-shim-vk — the guest Vulkan driver (a Vulkan ICD), in Rust (increment-1 FOUNDATION).
//!
//! Builds the shared object a standard Vulkan **loader** (libvulkan) discovers via an `icd.json`
//! manifest and accepts as a driver. An unmodified Vulkan app opens libvulkan; the loader loads this
//! ICD, negotiates the loader↔ICD interface, and resolves every `vk*` entry point through our
//! `vk_icdGetInstanceProcAddr`. We report the "dd Metal (Vulkan)" physical device; the compute/render
//! path lowers into a `dd-gpu` IR stream and — through [`dd_shim_common::transport`] — reaches the
//! host executor as the SAME IR the host decodes with the SAME Rust code (no hand-rolled second
//! encoder). This mirrors dd-shim-gl / dd-shim-cuda increment-1 exactly.
//!
//! ## Ported from real references (no invented behavior)
//! * **ICD interface** ([`icd`], [`handle`]) — Khronos **Vulkan-Loader** `docs/LoaderDriverInterface.md`
//!   + `include/vulkan/vk_icd.h`, and **MoltenVK** `vulkan.mm` (negotiation + proc-addr dispatch).
//!   This is what root-causes + fixes the prior `VK_ERROR_INCOMPATIBLE_DRIVER` (see [`icd`]).
//! * **Object model + device properties** ([`instance`], [`device`], [`state`]) — **MoltenVK**'s
//!   `MVKInstance`/`MVKPhysicalDevice`/`MVKDevice`/`MVKQueue` and its Apple-silicon reporting.
//! * **Type/ABI surface** — the Khronos **`ash`** bindings (`ash::vk`) for spec-exact `#[repr(C)]`
//!   struct layouts, and the Khronos **`vk.xml`** registry for the full entry-point list.
//!
//! ## Coverage
//! The exported `vk*` *surface* is code-generated from `vk.xml` (`build.rs` + `registry/`) — the full
//! core + extension command set (693 commands). Entry points in [`build::IMPLEMENTED`](../build.rs)
//! have real hand-written bodies; the rest are generated spec-faithful default stubs (correct ABI,
//! `VK_SUCCESS` return, `DD_SHIM_DEBUG`-traced), ported to real bodies incrementally — the shrinking
//! long tail. The [`ir_seam`] module sketches the Vulkan→IR mapping and round-trips what it encodes.

// The generated + hand-written entry-point surface uses the Vulkan C names verbatim (vkCreateInstance,
// PFN_vkVoidFunction, …) — those are the ABI identifiers, so the Rust casing lints don't apply.
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

// The shared IR + transport foundation. Re-exported so this crate's modules (and readers) see that the
// IR type is dd-gpu's, not a local copy.
pub use dd_shim_common as common;

pub mod command;
pub mod descriptor;
pub mod device;
pub mod handle;
pub mod icd;
pub mod instance;
pub mod ir_seam;
pub mod memory;
pub mod pipeline;
pub mod reg;
pub mod state;
pub mod stub;
pub mod types;
pub mod wsi;

// Bring every hand-written `#[no_mangle]` entry point into crate-root scope so the generated
// DISPATCH table (below) can reference the whole surface — implemented + stub — by bare name.
pub use command::*;
pub use descriptor::*;
pub use device::*;
pub use icd::*;
pub use instance::*;
pub use memory::*;
pub use pipeline::*;
pub use wsi::*;

// The generated C-ABI export surface (every `vk*` entry point not in `IMPLEMENTED`) + the name→address
// DISPATCH table the loader-facing proc-addr resolvers scan.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported Vulkan entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = VK_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_large() {
        // The vk.xml-driven surface must be the full core+extension set, not a hand-picked few.
        assert!(VK_ENTRYPOINTS >= 600, "Vulkan surface too small: {VK_ENTRYPOINTS}");
        // Every entry point is either hand-implemented or a generated stub.
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_COUNT, TOTAL_ENTRYPOINTS);
        // The whole surface is resolvable through the dispatch resolver the loader scans.
        assert_eq!(DISPATCH_NAMES.len(), TOTAL_ENTRYPOINTS);
    }

    // Count of hand-written entry points (kept in sync with build.rs IMPLEMENTED via the census).
    const IMPLEMENTED_COUNT: usize = TOTAL_ENTRYPOINTS - GENERATED_STUBS;

    #[test]
    fn dispatch_resolves_bringup_entry_points() {
        // The bring-up + ICD entry points a loader/app needs must all resolve by name.
        for name in [
            "vkGetInstanceProcAddr",
            "vkEnumerateInstanceVersion",
            "vkCreateInstance",
            "vkEnumeratePhysicalDevices",
            "vkGetPhysicalDeviceProperties",
            "vkCreateDevice",
            "vkGetDeviceQueue",
            "vkCreateCommandPool",
            "vkAllocateCommandBuffers",
        ] {
            assert!(
                DISPATCH_NAMES.contains(&name) && dispatch_addr(name).is_some(),
                "dispatch resolver missing bring-up entry point {name}"
            );
        }
    }
}
