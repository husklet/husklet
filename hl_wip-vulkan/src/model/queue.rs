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
        Self { family_index: super::instance::QUEUE_FAMILY_INDEX, queue_index: 0 }
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
        Self { ir_id, value: 0, signaled }
    }
}

// ---- WSI (the presentation engine the queue presents through) ------------------------------------
// Ported from `hl-shim-vk/src/wsi.rs` (`SurfaceRec`, `SwapchainRec`, `SwapImage`). Kept with the queue
// because `vkQueuePresentKHR` is a queue operation; the model file list has no separate WSI file.

use hl_gpu::protocol::model::enums::TextureFormat;

/// The reserved IR **texture** id every presentable swapchain image renders into: the host Metal
/// executor re-points texture id 1 at the current frame's IOSurface each frame
/// (`set_render_target(1, …)`). IR texture ids and buffer ids are separate host namespaces, so this
/// never collides with buffer id 1. Ported from `reg::PRESENT_IR_ID`.
pub const PRESENT_TEXTURE_ID: u32 = 1;

/// A `VkSurfaceKHR`: the backing hl-GPU IR surface id ([`hl_gpu::Cmd::CreateSurface`]) + geometry.
/// Mirrors `MVKSurface`.
#[derive(Clone, PartialEq, Debug)]
pub struct SurfaceRec {
    pub ir_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
}

/// One presentable swapchain image: the IR texture id it renders into ([`PRESENT_TEXTURE_ID`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SwapImage {
    pub ir_texture_id: u32,
}

/// A `VkSwapchainKHR`: the surface it presents through, its geometry/format, and its presentable
/// images. Mirrors `MVKSwapchain`.
#[derive(Clone, PartialEq, Debug)]
pub struct SwapchainRec {
    pub surface: crate::VkSurfaceKHR,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub images: Vec<SwapImage>,
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
