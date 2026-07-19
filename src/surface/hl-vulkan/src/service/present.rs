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
use crate::model::memory::ImageRec;
use crate::model::memory::{vk_format, Format};
use crate::model::queue::{
    ImageState, SurfaceCapabilities, SurfaceFormat, SurfaceRec, SwapImage, SwapchainRec,
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
pub struct QueueFamily(pub u32);

impl QueueFamily {
    pub fn supports_present(&self) -> bool {
        self.0 == QUEUE_FAMILY_INDEX
    }
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
    .map(|format| SurfaceFormat {
        format,
        color_space: VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
    })
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
    let format = Format(vk_format).wire();
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateSurface(
        ir_id,
        SurfaceDesc {
            width,
            height,
            format,
            hlp_surface,
        },
    )])?;
    dev.surfaces.insert(
        handle,
        SurfaceRec {
            ir_id,
            width,
            height,
            format,
        },
    );
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
        let s = dev.surfaces.get(&surface).ok_or(GpuError::Invalid(
            "vkCreateSwapchainKHR: unknown VkSurfaceKHR",
        ))?;
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
            ImageRec {
                ir_id: ir_texture_id,
                width,
                height,
                format,
                usage,
                sample_count: 1,
                is_render_target: true,
            },
        );
        images.push(SwapImage {
            ir_texture_id,
            handle,
            state: ImageState::Available,
        });
    }
    let handle = dev.alloc_handle();
    dev.swapchains.insert(
        handle,
        SwapchainRec {
            surface,
            width,
            height,
            format,
            images,
            acquire_cursor: 0,
        },
    );
    Ok(handle)
}

/// `vkGetSwapchainImagesKHR` — the `VkImage` handles for the swapchain's presentable images, in index
/// order. These are the SAME handles minted at [`create_swapchain`] (real Vulkan returns identical image
/// handles on every call); `vkAcquireNextImageKHR`'s returned index selects one of them. Errors on an
/// unknown swapchain.
impl Device {
    pub fn swapchain_images(&self, swapchain: VkSwapchainKHR) -> Result<Vec<VkImage>> {
        let sc = self.swapchains.get(&swapchain).ok_or(GpuError::Invalid(
            "vkGetSwapchainImagesKHR: unknown VkSwapchainKHR",
        ))?;
        Ok(sc.images.iter().map(|i| i.handle).collect())
    }

    /// `vkDestroySwapchainKHR` — retire a swapchain AND everything [`create_swapchain`] allocated for it: each
    /// presentable image's backing render-target texture (a [`Cmd::DestroyTexture`] frees the host texture, and
    /// its `dev.images` + `image_layouts` bookkeeping is dropped) and the swapchain's own presentation surface
    /// (a [`Cmd::DestroySurface`] frees it, and its `dev.surfaces` entry is dropped).
    ///
    /// Without retiring the images, a swapchain RECREATION — every window resize builds a fresh swapchain over
    /// the new extent and destroys the old one — would ORPHAN the old set's `ImageRec` entries (and the old
    /// swapchain's surface) in the device tables forever: a per-resize `VkImage`/surface handle leak that grows
    /// unbounded across a session of resizes. Retiring them here keeps `dev.images` holding EXACTLY the live
    /// swapchains' images. A no-op on an unknown / already-retired swapchain (`VK_NULL_HANDLE`); a still-live
    /// swapchain keeps all of its own images (only the destroyed one's are retired).
    pub fn destroy_swapchain(
        &mut self,
        sink: &mut dyn CommandSink,
        swapchain: VkSwapchainKHR,
    ) -> Result<()> {
        let Some(sc) = self.swapchains.remove(&swapchain) else {
            return Ok(()); // unknown / already retired — nothing to free (VK_NULL_HANDLE)
        };
        // Retire every presentable image: free its host texture, then drop its device-table bookkeeping so no
        // stale `VkImage` handle survives the swapchain.
        for img in &sc.images {
            sink.submit(&[Cmd::DestroyTexture(img.ir_texture_id)])?;
            self.images.remove(&img.handle);
            self.image_layouts.remove(&img.handle);
        }
        // Retire the swapchain's own presentation surface (minted per-swapchain at create time — the app's
        // instance-level `VkSurfaceKHR` is a different object, retired separately by `vkDestroySurfaceKHR`).
        if let Some(surf) = self.surfaces.remove(&sc.surface) {
            sink.submit(&[Cmd::DestroySurface(surf.ir_id)])?;
        }
        Ok(())
    }

    /// `vkAcquireNextImageKHR` — return the index of the next presentable image in genuine FIFO round-robin
    /// order. Starting at the swapchain's `acquire_cursor`, this scans the images cyclically for the first one
    /// in the pool ([`ImageState::Available`]), marks it [`ImageState::Acquired`] (so it is not handed out
    /// again until [`queue_present`] returns it to the pool), advances the cursor past it, and returns its
    /// index — so a real present loop cycles `0,1,..,N-1,0,..` instead of being pinned to image 0 (which would
    /// re-hand the app an image the presentation engine still owns, aborting the loop after one frame).
    ///
    /// If NO image is currently available (the app acquired more than it presented), the headless model —
    /// where a present completes immediately — would normally already have returned one; as a defensive
    /// fallback it returns the cursor image anyway (re-marking it acquired) so acquisition still makes forward
    /// progress rather than failing. Errors on an unknown or empty swapchain.
    ///
    /// The `_semaphore`/`_fence` the app may pass are signalled by the shim's existing sync path (unchanged);
    /// this driver-level entry only advances the acquire cursor and image ownership.
    pub fn acquire_next_image(&mut self, swapchain: VkSwapchainKHR) -> Result<u32> {
        let sc = self
            .swapchains
            .get_mut(&swapchain)
            .ok_or(GpuError::Invalid(
                "vkAcquireNextImageKHR: unknown VkSwapchainKHR",
            ))?;
        let count = sc.images.len();
        if count == 0 {
            return Err(GpuError::Invalid(
                "vkAcquireNextImageKHR: swapchain has no images",
            ));
        }
        let start = (sc.acquire_cursor as usize) % count;
        // Scan cyclically from the cursor for the first pool image; fall back to the cursor image itself.
        let index = (0..count)
            .map(|off| (start + off) % count)
            .find(|&i| sc.images[i].state == ImageState::Available)
            .unwrap_or(start);
        sc.images[index].state = ImageState::Acquired;
        sc.acquire_cursor = ((index + 1) % count) as u32;
        hl_log::hl_debug!(hl_log::tag::PRESENT, "acquire idx={} of={}", index, count);
        Ok(index as u32)
    }
}

/// `vkQueuePresentKHR` (one swapchain) — submit [`Cmd::Present`] naming the swapchain's surface + the
/// presented image's texture id, then RETURN that image to the pool. Because this headless present
/// completes immediately (the readback runs synchronously right after), the presented image is marked
/// [`ImageState::Available`] again as the last step, so the next `vkAcquireNextImageKHR` can round-robin
/// on to (and eventually back to) it — keeping a real FIFO present loop cycling. Errors on an unknown
/// swapchain / out-of-range image index.
pub fn queue_present(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    swapchain: VkSwapchainKHR,
    image_index: u32,
) -> Result<()> {
    let (surface_handle, texture) = {
        let sc = dev.swapchains.get(&swapchain).ok_or(GpuError::Invalid(
            "vkQueuePresentKHR: unknown VkSwapchainKHR",
        ))?;
        let img = sc
            .images
            .get(image_index as usize)
            .ok_or(GpuError::Invalid(
                "vkQueuePresentKHR: image index out of range",
            ))?;
        (sc.surface, img.ir_texture_id)
    };
    let surface = dev
        .surfaces
        .get(&surface_handle)
        .ok_or(GpuError::Invalid(
            "vkQueuePresentKHR: swapchain surface lost",
        ))?
        .ir_id;
    hl_log::hl_debug!(
        hl_log::tag::PRESENT,
        "present img={} surf={} tex={}",
        image_index,
        surface,
        texture
    );
    hl_log::hl_count!(hl_log::tag::PRESENT, "presents");
    sink.submit(&[Cmd::Present { surface, texture }])?;
    // The present engine is done with this image (immediate headless present) — return it to the pool so a
    // future acquire can hand it out again. Index was range-checked above, so the image is present.
    if let Some(img) = dev
        .swapchains
        .get_mut(&swapchain)
        .and_then(|sc| sc.images.get_mut(image_index as usize))
    {
        img.state = ImageState::Available;
    }
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
    let _readback_span = hl_log::hl_span!(hl_log::tag::PRESENT, "readback");
    hl_log::hl_debug!(
        hl_log::tag::PRESENT,
        "readback img={} {}x{}",
        image_index,
        width,
        height
    );
    let readback = dev.alloc_ir();
    let row_bytes = width as u64 * 4;
    let size = row_bytes * height as u64;
    sink.submit(&[
        Cmd::CreateBuffer(
            readback,
            BufferDesc {
                size,
                usage: buffer_usage::COPY_DST,
                label: String::new(),
            },
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

/// Read a presented swapchain image back and convert it to the `WL_SHM_FORMAT_XRGB8888` plane a `wl_shm`
/// buffer wants — the pixels `vkQueuePresentKHR` marshals onto the app's `wl_surface`.
///
/// Wraps [`read_presented_image`] (the device→host readback) with the format-aware
/// [`crate::adapter::wayland_app::pixels_to_xrgb8888`] convert: the readback is in the swapchain's native
/// texel order (`Bgra8`/`Rgba8`), and this reorders it into XRGB (channel-swapping RGBA, passing BGRA
/// through) with NO vertical flip (a Vulkan swapchain image is top-left origin). Returns the converted
/// plane plus the surface `(width, height)`.
pub fn read_presented_xrgb(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    swapchain: VkSwapchainKHR,
    image_index: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    use crate::adapter::wayland_app::pixels_to_xrgb8888;
    use hl_gpu::protocol::model::enums::TextureFormat;

    let (format, width, height) = {
        let sc = dev
            .swapchains
            .get(&swapchain)
            .ok_or(GpuError::Invalid("readback: unknown VkSwapchainKHR"))?;
        (sc.format, sc.width, sc.height)
    };
    let pixels = read_presented_image(dev, sink, swapchain, image_index)?;
    let source_is_bgra = matches!(format, TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb);
    let xrgb = pixels_to_xrgb8888(&pixels, width as usize, height as usize, source_is_bgra);
    Ok((xrgb, width, height))
}
