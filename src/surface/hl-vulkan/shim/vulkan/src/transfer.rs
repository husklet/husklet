//! IR-wired Vulkan transfer commands.
//!
//! Structurally invalid `void` commands are benign no-ops. Copy regions lower independently, buffer
//! writes flush at queue submission, and layout barriers record bookkeeping because the GPU IR is
//! layout-implicit.

use core::ffi::c_void;

use hl_gpu::protocol::model::descriptor::Mirror;
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::StateStore;
use crate::types::{Dispatchable, VkOffset3D};

mod barrier;
mod command;
mod copy;
mod copy2;

pub use barrier::*;
pub use command::*;
pub use copy::*;
pub use copy2::*;

/// One side of a `vkCmdBlitImage` region, normalized out of Vulkan's signed offset PAIR.
///
/// Vulkan gives a blit two corners rather than an origin and an extent, and `offsets[1]` before
/// `offsets[0]` on an axis is how it expresses a MIRROR. Normalizing with a min/max is unavoidable — the
/// IR's origin and extent are unsigned — so the comparison that the normalization performs is recorded
/// as `inverted` rather than thrown away. `depth` is carried unnormalized because a region spanning depth
/// is refused rather than mirrored.
pub(super) struct BlitRect {
    pub origin: (u32, u32),
    pub extent: (u32, u32),
    pub inverted: Mirror,
    pub depth: (i32, i32),
}

impl BlitRect {
    /// `None` for an EMPTY rect (zero extent on an axis) — nothing to do, and skipping it is right.
    pub fn of(offsets: &[VkOffset3D; 2]) -> Option<Self> {
        let [a, b] = offsets;
        if a.x == b.x || a.y == b.y {
            return None;
        }
        Some(BlitRect {
            origin: (a.x.min(b.x) as u32, a.y.min(b.y) as u32),
            extent: (a.x.abs_diff(b.x), a.y.abs_diff(b.y)),
            inverted: Mirror {
                x: b.x < a.x,
                y: b.y < a.y,
            },
            depth: (a.z, b.z),
        })
    }
}

/// Provides recording commands access to the logical device.
pub(super) struct ShimState;

impl ShimState {
    pub(super) fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        StateStore::with(|state| state.device_mut().map(f))
    }
}

/// Unwraps a dispatchable command buffer into its service handle.
pub(super) struct CommandBuffer;

impl CommandBuffer {
    pub(super) unsafe fn handle(command_buffer: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(command_buffer).map(|handle| *handle)
    }
}
