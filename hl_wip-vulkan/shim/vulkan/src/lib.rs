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
pub mod hostcopy;
pub mod icd;
pub mod instance;
pub mod maintenance;
pub mod state;
pub mod stub;
pub mod surface;
pub mod sync;
pub mod transfer;
pub mod types;
pub mod unsupported;

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
            // newly hand-written families resolve through the same table:
            "vkTrimCommandPool",
            "vkGetDeviceBufferMemoryRequirements",
            "vkSetPrivateData",
            "vkCreateSamplerYcbcrConversion",
            "vkCmdTraceRaysKHR",
            "vkCreateRenderPass2",
        ] {
            assert!(dispatch_addr(name).is_some(), "{name} does not resolve");
        }
    }

    // ---- hand-written maintenance / host-copy / not-supported bodies ------------------------------

    use crate::types::*;
    use core::ffi::c_void;

    #[test]
    fn device_buffer_memory_requirements_reports_size_and_alias_matches_base() {
        let ci = VkBufferCreateInfo {
            s_type: 0,
            p_next: core::ptr::null(),
            flags: 0,
            size: 4096,
            usage: 0,
            sharing_mode: 0,
            queue_family_index_count: 0,
            p_queue_family_indices: core::ptr::null(),
        };
        let info = VkDeviceBufferMemoryRequirements {
            s_type: 0,
            p_next: core::ptr::null(),
            p_create_info: &ci,
        };
        let mut base = VkMemoryRequirements2 {
            s_type: 0,
            p_next: core::ptr::null_mut(),
            memory_requirements: VkMemoryRequirements { size: 0, alignment: 0, memory_type_bits: 0 },
        };
        let mut khr = VkMemoryRequirements2 {
            s_type: 0,
            p_next: core::ptr::null_mut(),
            memory_requirements: VkMemoryRequirements { size: 0, alignment: 0, memory_type_bits: 0 },
        };
        crate::maintenance::vkGetDeviceBufferMemoryRequirements(
            core::ptr::null_mut(),
            &info as *const _ as *const c_void,
            &mut base as *mut _ as *mut c_void,
        );
        crate::maintenance::vkGetDeviceBufferMemoryRequirementsKHR(
            core::ptr::null_mut(),
            &info as *const _ as *const c_void,
            &mut khr as *mut _ as *mut c_void,
        );
        assert_eq!(base.memory_requirements.size, 4096);
        assert_eq!(base.memory_requirements.alignment, 256);
        // the KHR alias produces byte-identical output to the core body.
        assert_eq!(khr.memory_requirements.size, base.memory_requirements.size);
        assert_eq!(khr.memory_requirements.memory_type_bits, base.memory_requirements.memory_type_bits);
    }

    #[test]
    fn descriptor_set_layout_support_reports_supported() {
        let mut sup = VkDescriptorSetLayoutSupport { s_type: 0, p_next: core::ptr::null_mut(), supported: 0 };
        crate::maintenance::vkGetDescriptorSetLayoutSupport(
            core::ptr::null_mut(),
            core::ptr::null(),
            &mut sup as *mut _ as *mut c_void,
        );
        assert_eq!(sup.supported, VK_TRUE);
    }

    #[test]
    fn host_image_copy_is_honestly_unsupported() {
        // A device must exist (created below); a modeled `hostImageCopy` op returns the truthful error.
        let mut dev: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            crate::device::vkCreateDevice(core::ptr::null_mut(), core::ptr::null(), core::ptr::null(), &mut dev),
            VK_SUCCESS
        );
        let dummy = [0u8; 64];
        let r = crate::hostcopy::vkCopyMemoryToImage(dev, dummy.as_ptr() as *const c_void);
        assert_eq!(r, VK_ERROR_FEATURE_NOT_PRESENT);
        // the EXT alias matches the core body.
        assert_eq!(crate::hostcopy::vkCopyMemoryToImageEXT(dev, dummy.as_ptr() as *const c_void), r);
    }

    #[test]
    fn private_data_round_trips_and_ycbcr_conversion_creates() {
        let mut dev: *mut c_void = core::ptr::null_mut();
        assert_eq!(
            crate::device::vkCreateDevice(core::ptr::null_mut(), core::ptr::null(), core::ptr::null(), &mut dev),
            VK_SUCCESS
        );
        // private data: create a slot, store a value under an (objectType, handle), read it back.
        let mut slot: u64 = 0;
        assert_eq!(
            crate::maintenance::vkCreatePrivateDataSlot(dev, core::ptr::null(), core::ptr::null(), &mut slot),
            VK_SUCCESS
        );
        assert_ne!(slot, 0);
        assert_eq!(crate::maintenance::vkSetPrivateData(dev, 9, 0xABCD, slot, 0xDEAD_BEEF), VK_SUCCESS);
        let mut got: u64 = 0;
        crate::maintenance::vkGetPrivateData(dev, 9, 0xABCD, slot, &mut got);
        assert_eq!(got, 0xDEAD_BEEF);
        // an unset key reads back 0 (the spec default).
        let mut zero: u64 = 123;
        crate::maintenance::vkGetPrivateData(dev, 9, 0x0001, slot, &mut zero);
        assert_eq!(zero, 0);

        // ycbcr conversion: a non-null create info mints a live handle.
        let ycbcr_ci = [0u8; 64];
        let mut conv: u64 = 0;
        assert_eq!(
            crate::maintenance::vkCreateSamplerYcbcrConversion(
                dev,
                ycbcr_ci.as_ptr() as *const c_void,
                core::ptr::null(),
                &mut conv,
            ),
            VK_SUCCESS
        );
        assert_ne!(conv, 0);
        crate::maintenance::vkDestroySamplerYcbcrConversion(dev, conv, core::ptr::null());
    }

    #[test]
    fn ray_tracing_family_returns_extension_not_present() {
        // A wholesale-unmodeled extension command validates + returns the truthful, non-faked error.
        let mut pipe: u64 = 12345;
        let r = crate::unsupported::vkCreateRayTracingPipelinesKHR(
            core::ptr::null_mut(),
            0,
            0,
            0,
            core::ptr::null(),
            core::ptr::null(),
            &mut pipe as *mut u64 as *mut c_void,
        );
        assert_eq!(r, -7); // VK_ERROR_EXTENSION_NOT_PRESENT
    }
}
