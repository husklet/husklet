//! IR-wired Vulkan transfer commands.
//!
//! Structurally invalid `void` commands are benign no-ops. Copy regions lower independently, buffer
//! writes flush at queue submission, and layout barriers record bookkeeping because the GPU IR is
//! layout-implicit.

use core::ffi::c_void;

use hl_gpu::protocol::model::descriptor::{Extent3d, Mirror, Origin3d};
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
/// as `inverted` rather than thrown away.
///
/// All THREE axes are treated identically. Depth used to be carried as the raw `(a.z, b.z)` pair with a
/// caller-side refusal of anything but `(0, 1)`, which meant the z axis had no origin, no extent and no
/// flip — a shape that could only ever be refused. It is now normalized exactly as x and y are, so a
/// depth-spanning region is expressible and the depth flip rides in `inverted.z`.
pub(super) struct BlitRect {
    pub origin: Origin3d,
    pub extent: Extent3d,
    pub inverted: Mirror,
}

impl BlitRect {
    /// `None` for an EMPTY region (zero span on ANY axis) — nothing to do, and skipping it is right.
    ///
    /// z is included in that test rather than exempted from it. Vulkan requires a non-3D image's
    /// `srcOffsets` to be z = 0 and z = 1, so a legal 2D region always spans one slice and is unaffected;
    /// a zero z span is a degenerate region and is skipped for the same reason a zero-width one is.
    pub fn of(offsets: &[VkOffset3D; 2]) -> Option<Self> {
        let [a, b] = offsets;
        if a.x == b.x || a.y == b.y || a.z == b.z {
            return None;
        }
        Some(BlitRect {
            origin: Origin3d {
                x: a.x.min(b.x) as u32,
                y: a.y.min(b.y) as u32,
                z: a.z.min(b.z) as u32,
            },
            extent: Extent3d {
                width: a.x.abs_diff(b.x),
                height: a.y.abs_diff(b.y),
                depth: a.z.abs_diff(b.z),
            },
            inverted: Mirror {
                x: b.x < a.x,
                y: b.y < a.y,
                z: b.z < a.z,
            },
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
