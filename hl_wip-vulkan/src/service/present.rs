//! WSI — `vkCreateSurface`/`vkCreateSwapchainKHR`/`vkQueuePresentKHR` → the surface + present lowering.
//!
//! Ported from `hl-shim-vk/src/wsi.rs`. A surface lowers to one [`Cmd::CreateSurface`] (its IR surface
//! id is what a present targets). A swapchain's presentable images are REAL hl-GPU render-target
//! textures: [`create_swapchain`] emits one [`Cmd::CreateTexture`] per image (`RENDER_TARGET | COPY_SRC`,
//! sized/formatted from the surface) and mints a `VkImage` for each, so the app records rendering into an
//! acquired image exactly as into any [`crate::service::create::create_image`] target. A present lowers
//! to one [`Cmd::Present`] naming the surface + the presented image's real texture id; because that
//! texture is `COPY_SRC`-able, [`read_presented_image`] can copy it back to host pixels over the sink —
//! the same device→host path GL's `glReadPixels` uses (`CopyTextureToBuffer` + `read_buffer`).

use crate::model::instance::QUEUE_FAMILY_INDEX;
use crate::model::memory::{tex_format_from_vk, vk_format};
use crate::model::memory::ImageRec;
use crate::model::queue::{
    SurfaceCapabilities, SurfaceFormat, SurfaceRec, SwapImage, SwapchainRec,
    COMPOSITE_ALPHA_OPAQUE_BIT, CURRENT_EXTENT_UNDEFINED, SURFACE_IMAGE_USAGE,
    SURFACE_TRANSFORM_IDENTITY_BIT, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR, VK_PRESENT_MODE_FIFO_KHR,
};
use crate::*;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{BufferDesc, SurfaceDesc, TextureDesc};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, TextureDim};
use hl_gpu::{BufferId, Cmd, CommandBuffer, CommandSink, GpuError, Result};

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

/// `vkCreateSwapchainKHR` — create `image_count` REAL presentable images against `surface`. Each image is
/// a `Cmd::CreateTexture` render target (`RENDER_TARGET | COPY_SRC`, sized/formatted from the surface) with
/// its own `VkImage` handle registered in `dev.images`, so the app records rendering into an acquired
/// image exactly as into any other render target, a present names its texture, and its contents read back
/// over the sink. Errors on an unknown surface. (The surface's `CreateSurface` already went out.)
pub fn create_swapchain(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
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
    // A presentable image must be renderable (RENDER_TARGET, the app draws into it) AND copy-source-able
    // (COPY_SRC, so its presented contents read back to host pixels) — matching GL's default target
    // (`RENDER_TARGET | PRESENT | COPY_SRC`).
    let usage = texture_usage::RENDER_TARGET | texture_usage::COPY_SRC;
    let mut images = Vec::with_capacity(image_count.max(1) as usize);
    for _ in 0..image_count.max(1) {
        let ir_texture_id = dev.alloc_ir();
        sink.submit(&[Cmd::CreateTexture(
            ir_texture_id,
            TextureDesc {
                width,
                height,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format,
                usage,
                label: format!("swapimg{ir_texture_id}"),
            },
        )])?;
        let handle = dev.alloc_handle();
        dev.images.insert(
            handle,
            ImageRec { ir_id: ir_texture_id, width, height, format, usage, is_render_target: true },
        );
        images.push(SwapImage { ir_texture_id, handle });
    }
    let handle = dev.alloc_handle();
    dev.swapchains.insert(handle, SwapchainRec { surface, width, height, format, images });
    Ok(handle)
}

/// `vkGetSwapchainImagesKHR` — the `VkImage` handles for the swapchain's presentable images, in index
/// order. These are the SAME handles minted at [`create_swapchain`] (real Vulkan returns identical image
/// handles on every call); `vkAcquireNextImageKHR`'s returned index selects one of them. Errors on an
/// unknown swapchain.
pub fn get_swapchain_images(dev: &Device, swapchain: VkSwapchainKHR) -> Result<Vec<VkImage>> {
    let sc = dev
        .swapchains
        .get(&swapchain)
        .ok_or(GpuError::Invalid("vkGetSwapchainImagesKHR: unknown VkSwapchainKHR"))?;
    Ok(sc.images.iter().map(|i| i.handle).collect())
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

/// Read a swapchain image's pixels back to the host — the device→host readback of a presented image.
///
/// The app rendered into image `image_index` (a real `RENDER_TARGET | COPY_SRC` texture), so this copies
/// that whole texture into a fresh host-readable buffer with a `CopyTextureToBuffer` + [`CommandSink::
/// read_buffer`] — the SAME device→host port GL's `glReadPixels` uses (see
/// `hl-gl/src/service/readpixels.rs`) and hl-cuda's `cuMemcpyDtoH`. Returns the tight-packed plane
/// (`width*height*4`, top-left origin) in the image's native texel order (`Bgra8`/`Rgba8` per the surface
/// format). This is the driver-level readback a live compositor present-marshalling path would drive over
/// the presented image; here it PROVES the presented swapchain image is real + readable end-to-end.
pub fn read_presented_image(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    swapchain: VkSwapchainKHR,
    image_index: u32,
) -> Result<Vec<u8>> {
    let (texture, width, height) = {
        let sc = dev
            .swapchains
            .get(&swapchain)
            .ok_or(GpuError::Invalid("readback: unknown VkSwapchainKHR"))?;
        let img = sc
            .images
            .get(image_index as usize)
            .ok_or(GpuError::Invalid("readback: image index out of range"))?;
        (img.ir_texture_id, sc.width, sc.height)
    };
    let readback = dev.alloc_ir();
    let row_bytes = width as u64 * 4;
    let size = row_bytes * height as u64;
    sink.submit(&[
        Cmd::CreateBuffer(
            readback,
            BufferDesc { size, usage: buffer_usage::COPY_DST, label: String::new() },
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyTextureToBuffer {
                src: texture,
                mip: 0,
                width,
                height,
                dst: readback,
                dst_offset: 0,
                bytes_per_row: row_bytes as u32,
            }],
            signal: None,
        }),
    ])?;
    sink.read_buffer(BufferId(readback), 0, size as usize)
}
