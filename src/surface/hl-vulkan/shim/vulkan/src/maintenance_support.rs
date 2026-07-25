use core::ffi::c_void;

use hl_vulkan::Device;

use crate::state::StateStore;
use crate::types::*;

pub(super) struct ShimState;

impl ShimState {
    pub(super) fn with_device_result(f: impl FnOnce(&mut Device) -> VkResult) -> VkResult {
        StateStore::with(|state| state.device.as_mut().map(f))
            .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
    }
}

pub(super) struct CommandBuffer;

impl CommandBuffer {
    pub(super) unsafe fn handle(pointer: *mut c_void) -> Option<u64> {
        Dispatchable::<u64>::inner(pointer).map(|handle| *handle)
    }
}

pub(super) struct MemoryRequirements;

impl MemoryRequirements {
    pub(super) fn write(output: *mut c_void, size: u64) {
        if let Some(output) = unsafe { (output as *mut VkMemoryRequirements2).as_mut() } {
            let memory_type_bits =
                StateStore::with(|state| state.physical_device().all_memory_type_bits());
            output.memory_requirements = VkMemoryRequirements {
                size,
                alignment: 256,
                memory_type_bits,
            };
        }
    }
}

pub(super) struct SparseRequirements;

impl SparseRequirements {
    pub(super) fn write_empty(count: *mut u32) {
        if let Some(count) = unsafe { count.as_mut() } {
            *count = 0;
        }
    }
}
