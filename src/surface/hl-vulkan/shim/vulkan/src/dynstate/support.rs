use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::{Device, VkCommandBuffer};

use crate::state::StateStore;
use crate::types::Dispatchable;

pub(super) struct ShimState;

impl ShimState {
    pub(super) fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        StateStore::with(|state| state.device.as_mut().map(f))
    }
}

pub(super) struct CommandBuffer;

impl CommandBuffer {
    pub(super) unsafe fn handle(pointer: *mut c_void) -> Option<VkCommandBuffer> {
        Dispatchable::<VkCommandBuffer>::inner(pointer).map(|handle| *handle)
    }
}

pub(super) struct DynamicState;

impl DynamicState {
    pub(super) fn record(
        command_buffer: *mut c_void,
        update: impl FnOnce(&mut hl_vulkan::model::command::DynamicState),
    ) {
        let Some(handle) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
            return;
        };
        ShimState::with_device(|device| {
            let _ = record::set_dynamic(device, handle, update);
        });
    }
}
