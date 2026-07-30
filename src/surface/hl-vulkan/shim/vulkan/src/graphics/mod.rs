//! Vulkan graphics C-ABI adapters.
//!
//! The generated resolver sees one flat command surface through these re-exports. Implementation modules
//! own the Vulkan object or command family they marshal into `hl_vulkan` services.

use core::ffi::{c_char, c_void};

use hl_gpu::protocol::model::descriptor::{
    BlendState, DepthState, StencilFaceState, TextureViewDesc, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{compare, TextureAspect, TextureDim, TextureFormat, Topology};
use hl_gpu::{Cmd, CommandSink};
use hl_vulkan::adapter::wayland_app::WaylandAppPresenter;
use hl_vulkan::model::memory::Format;
use hl_vulkan::result::Status;
use hl_vulkan::service::record::{RenderingColorAttachment, RenderingDepthAttachment};
use hl_vulkan::service::{create, present, record};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::{RenderPassDepth, RenderPassRec, StateStore};
use crate::types::*;

mod draw;
mod image;
mod pass;
mod pipeline;
mod render;
mod state;
mod swapchain;

pub use draw::*;
pub use image::*;
pub use pass::*;
pub use pipeline::*;
pub use render::*;
pub use state::*;
pub use swapchain::*;

pub(super) struct ShimState;

impl ShimState {
    pub(super) fn with_sink<R>(
        f: impl FnOnce(&mut Device, &mut dyn CommandSink) -> R,
    ) -> Option<R> {
        StateStore::with(|state| {
            let sink = &mut state.sink;
            let device = state.device.as_mut()?;
            Some(f(device, sink))
        })
    }

    pub(super) fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        StateStore::with(|state| state.device.as_mut().map(f))
    }
}

pub(super) struct EntryPoint;

impl EntryPoint {
    /// Read an entry point, falling back to Vulkan's conventional `main` on absent or invalid text.
    pub(super) unsafe fn read<'a>(pointer: *const c_char) -> &'a str {
        if pointer.is_null() {
            return "main";
        }
        core::ffi::CStr::from_ptr(pointer)
            .to_str()
            .unwrap_or("main")
    }
}

pub(super) struct CommandBuffer;

impl CommandBuffer {
    pub(super) unsafe fn handle(pointer: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(pointer).copied()
    }
}
