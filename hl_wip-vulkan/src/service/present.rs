//! WSI — `vkCreateSurface`/`vkCreateSwapchainKHR`/`vkQueuePresentKHR` → the surface + present lowering.
//!
//! Ported from `hl-shim-vk/src/wsi.rs`. A surface lowers to one [`Cmd::CreateSurface`] (its IR surface
//! id is what a present targets). Swapchain images are host-owned render targets: they render into the
//! reserved present texture id ([`crate::model::queue::PRESENT_TEXTURE_ID`], which the host executor
//! re-points at the current frame's IOSurface each frame), so the swapchain emits NO `CreateTexture`.
//! A present lowers to one [`Cmd::Present`] naming the surface + the presented image's texture id.

use crate::model::memory::tex_format_from_vk;
use crate::model::queue::{SurfaceRec, SwapImage, SwapchainRec, PRESENT_TEXTURE_ID};
use crate::*;
use hl_gpu::protocol::model::descriptor::SurfaceDesc;
use hl_gpu::{Cmd, CommandSink, GpuError, Result};

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
