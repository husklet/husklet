//! IR-wired Vulkan transfer commands.
//!
//! Structurally invalid `void` commands are benign no-ops. Copy regions lower independently, buffer
//! writes flush at queue submission, and layout barriers record bookkeeping because the GPU IR is
//! layout-implicit.

use core::ffi::c_void;

use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::StateStore;
use crate::types::Dispatchable;

mod barrier;
mod command;
mod copy;
mod copy2;

pub use barrier::*;
pub use command::*;
pub use copy::*;
pub use copy2::*;

/// Provides recording commands access to the logical device.
pub(super) struct ShimState;

impl ShimState {
    pub(super) fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        StateStore::with(|state| state.device.as_mut().map(f))
    }
}

/// Unwraps a dispatchable command buffer into its service handle.
pub(super) struct CommandBuffer;

impl CommandBuffer {
    pub(super) unsafe fn handle(command_buffer: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(command_buffer).map(|handle| *handle)
    }
}
