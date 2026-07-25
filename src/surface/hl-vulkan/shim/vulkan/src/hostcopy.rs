//! Host image copy (`VK_EXT_host_image_copy` / core 1.4) + the maintenance5 subresource-layout2 query.
//!
//! The modeled `VkImage` is a host-owned GPU render/transfer target whose texels live on the executor,
//! NOT as CPU-visible bytes in the guest — so a CPU-side `memory <-> image` transfer cannot be honestly
//! represented. The `hostImageCopy` feature is therefore NOT advertised (advertise-only-what's-real), and
//! these entry points are truthful HAND-WRITTEN not-supported bodies: they validate the device + argument
//! pointer and return `VK_ERROR_FEATURE_NOT_PRESENT` (never a false `VK_SUCCESS` that silently drops the
//! copy). `vkGetImageSubresourceLayout2` IS real — it delegates to the modeled linear-layout query.

use core::ffi::c_void;

use crate::state::StateStore;
use crate::types::*;

/// Whether a logical device exists (host-copy commands need one). `false` maps to
/// `VK_ERROR_INITIALIZATION_FAILED`.
fn have_device() -> bool {
    StateStore::with(|s| s.device.is_some())
}

/// The shared truthful answer for an unmodeled host-image-copy op: validate the device + the argument
/// pointer, then report the honest `VK_ERROR_FEATURE_NOT_PRESENT` (the `hostImageCopy` feature is not
/// advertised — the executor holds image texels host-side, not as guest-CPU bytes).
struct HostCopy;
impl HostCopy {
    fn unsupported(cmd: &'static str, info: *const c_void) -> VkResult {
        if info.is_null() {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        if !have_device() {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        crate::stub::Call::unsupported(
            cmd,
            "hostImageCopy feature is not advertised (image texels are host-owned)",
        );
        VK_ERROR_FEATURE_NOT_PRESENT
    }
}

#[no_mangle]
pub extern "C" fn vkCopyMemoryToImage(
    _device: *mut c_void,
    p_copy_memory_to_image_info: *const c_void,
) -> VkResult {
    HostCopy::unsupported("vkCopyMemoryToImage", p_copy_memory_to_image_info)
}

#[no_mangle]
pub extern "C" fn vkCopyMemoryToImageEXT(
    device: *mut c_void,
    p_copy_memory_to_image_info: *const c_void,
) -> VkResult {
    vkCopyMemoryToImage(device, p_copy_memory_to_image_info)
}

#[no_mangle]
pub extern "C" fn vkCopyImageToMemory(
    _device: *mut c_void,
    p_copy_image_to_memory_info: *const c_void,
) -> VkResult {
    HostCopy::unsupported("vkCopyImageToMemory", p_copy_image_to_memory_info)
}

#[no_mangle]
pub extern "C" fn vkCopyImageToMemoryEXT(
    device: *mut c_void,
    p_copy_image_to_memory_info: *const c_void,
) -> VkResult {
    vkCopyImageToMemory(device, p_copy_image_to_memory_info)
}

#[no_mangle]
pub extern "C" fn vkCopyImageToImage(
    _device: *mut c_void,
    p_copy_image_to_image_info: *const c_void,
) -> VkResult {
    HostCopy::unsupported("vkCopyImageToImage", p_copy_image_to_image_info)
}

#[no_mangle]
pub extern "C" fn vkCopyImageToImageEXT(
    device: *mut c_void,
    p_copy_image_to_image_info: *const c_void,
) -> VkResult {
    vkCopyImageToImage(device, p_copy_image_to_image_info)
}

#[no_mangle]
pub extern "C" fn vkTransitionImageLayout(
    _device: *mut c_void,
    _transition_count: u32,
    p_transitions: *const c_void,
) -> VkResult {
    HostCopy::unsupported("vkTransitionImageLayout", p_transitions)
}

#[no_mangle]
pub extern "C" fn vkTransitionImageLayoutEXT(
    device: *mut c_void,
    transition_count: u32,
    p_transitions: *const c_void,
) -> VkResult {
    vkTransitionImageLayout(device, transition_count, p_transitions)
}

// ==================================================================================================
// vkGetImageSubresourceLayout2 (maintenance5 / core 1.4) — a REAL query
// ==================================================================================================

/// `vkGetImageSubresourceLayout2(EXT/KHR)` — report the linear byte layout of `image`'s base subresource
/// (the modeled images are single-mip 2D RGBA8 targets: `rowPitch = width*4`). Leaves the output zeroed on
/// an unknown image. This is the promoted `...2` form of the implemented `vkGetImageSubresourceLayout`.
#[no_mangle]
pub extern "C" fn vkGetImageSubresourceLayout2(
    _device: *mut c_void,
    image: u64,
    _p_subresource: *const c_void,
    p_layout: *mut c_void,
) {
    let Some(out) = (unsafe { (p_layout as *mut VkSubresourceLayout2).as_mut() }) else {
        return;
    };
    out.subresource_layout = VkSubresourceLayout::default();
    if let Some(Ok(l)) =
        StateStore::with(|s| s.device.as_ref().map(|d| d.image_subresource_layout(image)))
    {
        out.subresource_layout = VkSubresourceLayout {
            offset: l.offset,
            size: l.size,
            row_pitch: l.row_pitch,
            array_pitch: l.array_pitch,
            depth_pitch: l.depth_pitch,
        };
    }
}

#[no_mangle]
pub extern "C" fn vkGetImageSubresourceLayout2EXT(
    device: *mut c_void,
    image: u64,
    p_subresource: *const c_void,
    p_layout: *mut c_void,
) {
    vkGetImageSubresourceLayout2(device, image, p_subresource, p_layout)
}

#[no_mangle]
pub extern "C" fn vkGetImageSubresourceLayout2KHR(
    device: *mut c_void,
    image: u64,
    p_subresource: *const c_void,
    p_layout: *mut c_void,
) {
    vkGetImageSubresourceLayout2(device, image, p_subresource, p_layout)
}
