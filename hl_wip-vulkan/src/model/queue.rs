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
