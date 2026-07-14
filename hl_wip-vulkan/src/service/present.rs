//! WSI — `vkCreateSurface`/`vkCreateSwapchainKHR`/`vkQueuePresentKHR` → the surface + present lowering.
//!
//! Ported from `hl-shim-vk/src/wsi.rs`. A surface lowers to one [`Cmd::CreateSurface`] (its IR surface
//! id is what a present targets). Swapchain images are host-owned render targets: they render into the
//! reserved present texture id ([`crate::model::queue::PRESENT_TEXTURE_ID`], which the host executor
//! re-points at the current frame's IOSurface each frame), so the swapchain emits NO `CreateTexture`.
//! A present lowers to one [`Cmd::Present`] naming the surface + the presented image's texture id.

use crate::model::instance::QUEUE_FAMILY_INDEX;
use crate::model::memory::{tex_format_from_vk, vk_format};
use crate::model::queue::{
    SurfaceCapabilities, SurfaceFormat, SurfaceRec, SwapImage, SwapchainRec,
    COMPOSITE_ALPHA_OPAQUE_BIT, CURRENT_EXTENT_UNDEFINED, PRESENT_TEXTURE_ID, SURFACE_IMAGE_USAGE,
    SURFACE_TRANSFORM_IDENTITY_BIT, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR, VK_PRESENT_MODE_FIFO_KHR,
};
use crate::*;
use hl_gpu::protocol::model::descriptor::SurfaceDesc;
use hl_gpu::{Cmd, CommandSink, GpuError, Result};

// ---- WSI physical-device surface queries (modeled, physical-device-level — no Device/sink) --------

/// `vkGetPhysicalDeviceSurfaceSupportKHR` — whether `queue_family_index` can present. The lone family
/// (graphics+compute+transfer) is the present family; any other index cannot present.
pub fn surface_supports_present(queue_family_index: u32) -> bool {
    queue_family_index == QUEUE_FAMILY_INDEX
}

/// `vkGetPhysicalDeviceSurfaceCapabilitiesKHR` — the modeled surface capabilities (double/triple
/// buffered, surface-defined extent, identity transform, opaque alpha). Ported from
/// `hl-shim-vk/src/wsi.rs::surface_capabilities`.
pub fn surface_capabilities() -> SurfaceCapabilities {
    SurfaceCapabilities {
        min_image_count: 2,
        max_image_count: 3,
        current_extent: CURRENT_EXTENT_UNDEFINED,
        min_image_extent: (1, 1),
        max_image_extent: (16384, 16384),
        max_image_array_layers: 1,
        supported_transforms: SURFACE_TRANSFORM_IDENTITY_BIT,
        current_transform: SURFACE_TRANSFORM_IDENTITY_BIT,
        supported_composite_alpha: COMPOSITE_ALPHA_OPAQUE_BIT,
        supported_usage_flags: SURFACE_IMAGE_USAGE,
    }
}

/// `vkGetPhysicalDeviceSurfaceFormatsKHR` — the presentable formats: BGRA8 / RGBA8, UNORM + SRGB, all in
/// the SRGB-nonlinear color space (the color subset the render/transfer path materializes; BGRA8 is what
/// the hl-display dma-buf present expects). Ported from `hl-shim-vk/src/wsi.rs::SURFACE_FORMAT` (widened
/// to the full 4-format color subset).
pub fn surface_formats() -> Vec<SurfaceFormat> {
    [
        vk_format::B8G8R8A8_UNORM,
        vk_format::B8G8R8A8_SRGB,
        vk_format::R8G8B8A8_UNORM,
        vk_format::R8G8B8A8_SRGB,
    ]
    .into_iter()
    .map(|format| SurfaceFormat { format, color_space: VK_COLOR_SPACE_SRGB_NONLINEAR_KHR })
    .collect()
}

/// `vkGetPhysicalDeviceSurfacePresentModesKHR` — the supported present modes: FIFO (the always-available,
/// v-synced mode the compositor present path implements).
pub fn surface_present_modes() -> Vec<i32> {
    vec![VK_PRESENT_MODE_FIFO_KHR]
}

/// `vkCreate*SurfaceKHR` — mint an hl-GPU surface id and submit [`Cmd::CreateSurface`]. `hlp_surface`
/// is the HLP surface id this GPU surface presents through (the compositor's window surface).
pub fn create_surface(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    width: u32,
    height: u32,
    vk_format: u32,
    hlp_surface: u32,
) -> Result<VkSurfaceKHR> {
    let format = tex_format_from_vk(vk_format);
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateSurface(ir_id, SurfaceDesc { width, height, format, hlp_surface })])?;
    dev.surfaces.insert(handle, SurfaceRec { ir_id, width, height, format });
    Ok(handle)
}

/// `vkCreateSwapchainKHR` — register `image_count` presentable images against `surface`, each rendering
/// into the reserved present texture id. Errors on an unknown surface. No IR is emitted (the images are
/// host-owned render targets; the surface's `CreateSurface` already went out).
pub fn create_swapchain(
    dev: &mut Device,
    surface: VkSurfaceKHR,
    image_count: u32,
) -> Result<VkSwapchainKHR> {
    let (width, height, format) = {
        let s = dev
            .surfaces
            .get(&surface)
            .ok_or(GpuError::Invalid("vkCreateSwapchainKHR: unknown VkSurfaceKHR"))?;
        (s.width, s.height, s.format)
    };
    let images = (0..image_count.max(1))
        .map(|_| SwapImage { ir_texture_id: PRESENT_TEXTURE_ID })
        .collect();
    let handle = dev.alloc_handle();
    dev.swapchains.insert(handle, SwapchainRec { surface, width, height, format, images });
    Ok(handle)
}

/// `vkAcquireNextImageKHR` — return the index of a presentable image. The bring-up model is a trivial
/// single-image acquire (index 0); real FIFO round-robin is a later hardening pass.
pub fn acquire_next_image(dev: &Device, swapchain: VkSwapchainKHR) -> Result<u32> {
    let sc = dev
        .swapchains
        .get(&swapchain)
        .ok_or(GpuError::Invalid("vkAcquireNextImageKHR: unknown VkSwapchainKHR"))?;
    if sc.images.is_empty() {
        return Err(GpuError::Invalid("vkAcquireNextImageKHR: swapchain has no images"));
    }
    Ok(0)
}

/// `vkQueuePresentKHR` (one swapchain) — submit [`Cmd::Present`] naming the swapchain's surface + the
/// presented image's texture id. Errors on an unknown swapchain / out-of-range image index.
pub fn queue_present(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    swapchain: VkSwapchainKHR,
    image_index: u32,
) -> Result<()> {
    let (surface_handle, texture) = {
        let sc = dev
            .swapchains
            .get(&swapchain)
            .ok_or(GpuError::Invalid("vkQueuePresentKHR: unknown VkSwapchainKHR"))?;
        let img = sc
            .images
            .get(image_index as usize)
            .ok_or(GpuError::Invalid("vkQueuePresentKHR: image index out of range"))?;
        (sc.surface, img.ir_texture_id)
    };
    let surface = dev
        .surfaces
        .get(&surface_handle)
        .ok_or(GpuError::Invalid("vkQueuePresentKHR: swapchain surface lost"))?
        .ir_id;
    sink.submit(&[Cmd::Present { surface, texture }])?;
    Ok(())
}
