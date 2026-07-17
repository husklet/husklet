//! The `VkQueue` + `VkFence` model — the submission target + the host-blocking sync primitive.
//!
//! Ported from `hl-shim-vk/src/{state.rs,reg.rs}` (`Queue`, `FenceRec`). The lone queue (graphics +
//! compute + transfer) is where `vkQueueSubmit`/`vkQueuePresentKHR` land. A `VkFence` carries a backing
//! hl-GPU fence id so [`crate::service::submit`] can signal it on a `Cmd::Submit` and block on
//! [`hl_gpu::CommandSink::wait`] — the same timeline-fence barrier hl-cuda's `synchronize` uses.

/// A `VkQueue`: which family/index it was retrieved as. Mirrors `MVKQueue`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Queue {
    pub family_index: u32,
    pub queue_index: u32,
}

impl Queue {
    /// The lone queue of the single family (index 0), the one `vkGetDeviceQueue(0, 0)` returns.
    pub fn primary() -> Self {
        Self {
            family_index: super::instance::QUEUE_FAMILY_INDEX,
            queue_index: 0,
        }
    }
}

/// A `VkFence`: its backing hl-GPU fence id, the timeline value it is signalled at, and its guest-side
/// signaled state. Mirrors `MVKFence` (a guest-side state machine over the host timeline fence).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FenceRec {
    pub ir_id: u32,
    /// The timeline value the guarding submit signals this fence at (0 until first submitted).
    pub value: u64,
    pub signaled: bool,
}

impl FenceRec {
    /// A fence freshly created by `vkCreateFence`. `signaled` reflects `VK_FENCE_CREATE_SIGNALED_BIT`.
    pub fn new(ir_id: u32, signaled: bool) -> Self {
        Self {
            ir_id,
            value: 0,
            signaled,
        }
    }
}

// ---- WSI (the presentation engine the queue presents through) ------------------------------------
// Ported from `hl-shim-vk/src/wsi.rs` (`SurfaceRec`, `SwapchainRec`, `SwapImage`). Kept with the queue
// because `vkQueuePresentKHR` is a queue operation; the model file list has no separate WSI file.

use hl_gpu::protocol::model::enums::TextureFormat;

/// A `VkSurfaceKHR`: the backing hl-GPU IR surface id ([`hl_gpu::Cmd::CreateSurface`]) + geometry.
/// Mirrors `MVKSurface`.
#[derive(Clone, PartialEq, Debug)]
pub struct SurfaceRec {
    pub ir_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
}

/// Where a swapchain image is in the acquire→render→present→available cycle — the per-image ownership
/// the FIFO round-robin acquire tracks. Mirrors the `VkSwapchainImage` availability a real presentation
/// engine keeps: an image the app holds (`Acquired`) or has handed back to present (`Presented`) must not
/// be re-acquired until it returns to the pool (`Available`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ImageState {
    /// In the pool — free for `vkAcquireNextImageKHR` to hand out.
    #[default]
    Available,
    /// Handed to the app by `vkAcquireNextImageKHR`; the app is rendering into it, not yet presented.
    Acquired,
    /// Handed to `vkQueuePresentKHR`; owned by the presentation engine. In this headless model the
    /// present completes immediately, so it returns to `Available` at the end of the present.
    Presented,
}

/// One presentable swapchain image. Unlike the old reserved-id model (every image aliased a single
/// host-owned present texture), each image is now backed by a REAL hl-GPU render-target texture
/// ([`hl_gpu::Cmd::CreateTexture`] with `RENDER_TARGET | COPY_SRC`, emitted at
/// [`crate::service::present::create_swapchain`]) — so the app renders into it like any other image, a
/// present names its texture id, and a `CopyTextureToBuffer` + `read_buffer` reads its pixels back to the
/// host (the same device→host path GL's `glReadPixels` uses). `handle` is the `VkImage`
/// `vkGetSwapchainImagesKHR` hands the app for this image; `ir_texture_id` is that image's backing texture;
/// `state` is its acquire-cycle ownership (starts [`ImageState::Available`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SwapImage {
    pub ir_texture_id: u32,
    pub handle: crate::VkImage,
    pub state: ImageState,
}

/// A `VkSwapchainKHR`: the surface it presents through, its geometry/format, its presentable images, and
/// the round-robin acquire cursor. Mirrors `MVKSwapchain`. `acquire_cursor` is the next index
/// `vkAcquireNextImageKHR` scans from, so successive acquires cycle `0,1,..,N-1,0,..` instead of always
/// returning image 0 (which would re-hand the app an image the presentation engine still owns).
#[derive(Clone, PartialEq, Debug)]
pub struct SwapchainRec {
    pub surface: crate::VkSurfaceKHR,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub images: Vec<SwapImage>,
    /// The index the next round-robin acquire starts its scan at (mod `images.len()`).
    pub acquire_cursor: u32,
}

// ---- WSI physical-device surface queries (modeled values) ----------------------------------------
// `vkGetPhysicalDeviceSurface{Support,Capabilities,Formats,PresentModes}KHR` report these fixed,
// truthful values for the modeled presentation engine. Ported from `hl-shim-vk/src/wsi.rs`
// (`surface_capabilities`, `SURFACE_FORMAT`, FIFO). Raw Vulkan enum/flag values (from vk.xml) so the
// shim copies them straight into the app's C structs.

/// `VkColorSpaceKHR::SRGB_NONLINEAR` — the one color space the surface formats advertise.
pub const VK_COLOR_SPACE_SRGB_NONLINEAR_KHR: i32 = 0;
/// `VkPresentModeKHR::FIFO` — the one present mode (guaranteed-available, v-synced).
pub const VK_PRESENT_MODE_FIFO_KHR: i32 = 2;
/// `VkSurfaceTransformFlagBitsKHR::IDENTITY`.
pub const SURFACE_TRANSFORM_IDENTITY_BIT: u32 = 0x0000_0001;
/// `VkCompositeAlphaFlagBitsKHR::OPAQUE`.
pub const COMPOSITE_ALPHA_OPAQUE_BIT: u32 = 0x0000_0001;
/// `VkImageUsageFlagBits` a swapchain image supports: COLOR_ATTACHMENT | TRANSFER_SRC | TRANSFER_DST.
pub const SURFACE_IMAGE_USAGE: u32 = 0x0000_0010 | 0x0000_0001 | 0x0000_0002;
/// The special "surface decides" current extent, both dimensions `u32::MAX` (the app must pick).
pub const CURRENT_EXTENT_UNDEFINED: (u32, u32) = (u32::MAX, u32::MAX);

/// The modeled `VkSurfaceCapabilitiesKHR` (min/max image count, extents, transforms, usage). Values
/// ported from `hl-shim-vk/src/wsi.rs::surface_capabilities` (MoltenVK Apple-class WSI).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SurfaceCapabilities {
    pub min_image_count: u32,
    pub max_image_count: u32,
    /// `(u32::MAX, u32::MAX)` == surface-defined; the app chooses the swapchain extent within bounds.
    pub current_extent: (u32, u32),
    pub min_image_extent: (u32, u32),
    pub max_image_extent: (u32, u32),
    pub max_image_array_layers: u32,
    pub supported_transforms: u32,
    pub current_transform: u32,
    pub supported_composite_alpha: u32,
    pub supported_usage_flags: u32,
}

/// One modeled `VkSurfaceFormatKHR` (raw `VkFormat` + `VkColorSpaceKHR`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SurfaceFormat {
    /// Raw `VkFormat`.
    pub format: u32,
    /// Raw `VkColorSpaceKHR`.
    pub color_space: i32,
}
