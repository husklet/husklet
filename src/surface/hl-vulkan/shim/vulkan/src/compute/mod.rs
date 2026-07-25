//! Vulkan compute, descriptor, memory, submission, and synchronization C-ABI adapters.
//!
//! Public re-exports preserve the flat command surface consumed by the generated resolver.

use core::ffi::{c_char, c_void};

use hl_gpu::CommandSink;
use hl_vulkan::model::descriptor::{DescriptorTemplateEntry, DescriptorType, LayoutBinding};
use hl_vulkan::result::{Status, VK_ERROR_OUT_OF_POOL_MEMORY};
use hl_vulkan::service::{create, record, submit as submit_service};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::StateStore;
use crate::types::*;

mod command;
mod descriptor;
mod fence;
mod memory;
mod pipeline;
mod submit;

pub use command::*;
pub use descriptor::*;
pub use fence::*;
pub use memory::*;
pub use pipeline::*;
pub use submit::*;

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
}

pub(super) struct ResultStatus;

impl ResultStatus {
    pub(super) fn from_gpu(result: hl_gpu::Result<()>) -> VkResult {
        match result {
            Ok(()) => VK_SUCCESS,
            Err(error) => Status::from_error(&error),
        }
    }
}

pub(super) struct CommandBuffer;

impl CommandBuffer {
    pub(super) fn from_handle(handle: VkCbHandle) -> *mut c_void {
        Dispatchable::new(handle)
    }

    pub(super) unsafe fn handle(pointer: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(pointer).copied()
    }
}

pub(super) struct ExtensionChain;

impl ExtensionChain {
    pub(super) unsafe fn find(mut pointer: *const c_void, target: i32) -> *const c_void {
        while let Some(base) = (pointer as *const VkBaseInStructure).as_ref() {
            if base.s_type == target {
                return pointer;
            }
            pointer = base.p_next as *const c_void;
        }
        core::ptr::null()
    }
}

pub(super) struct EntryPoint;

impl EntryPoint {
    pub(super) unsafe fn read<'a>(pointer: *const c_char) -> &'a str {
        if pointer.is_null() {
            return "main";
        }
        core::ffi::CStr::from_ptr(pointer)
            .to_str()
            .unwrap_or("main")
    }
}
