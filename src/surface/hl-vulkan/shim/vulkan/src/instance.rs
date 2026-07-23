//! Instance and physical-device bring-up for the guest Vulkan ICD.
//!
//! This module reports the Vulkan capabilities backed by the host Metal executor. It emits no IR.

use core::ffi::{c_char, c_void};

use hl_vulkan::model::capability::{self, ExtensionProp};
use hl_vulkan::model::memory::Format;
use hl_vulkan::Instance;

use crate::state::StateStore;
use crate::types::*;

mod format;
mod lifecycle;
mod limits;
mod physical;
mod properties;

pub use format::*;
pub use lifecycle::*;
use limits::metal_limits;
pub use physical::*;
pub use properties::*;

#[cfg(test)]
mod tests;

/// A nul-terminated Vulkan name stored in a fixed-size C character array.
struct Name;

impl Name {
    fn write(destination: &mut [c_char], name: &str) {
        destination.fill(0);
        let bytes = name.as_bytes();
        let length = bytes.len().min(destination.len().saturating_sub(1));
        for (destination, byte) in destination.iter_mut().zip(bytes).take(length) {
            *destination = *byte as c_char;
        }
    }
}

/// Writes Vulkan's two-call enumeration result.
///
/// # Safety
/// `count` must be writable. Non-null `data` must address `*count` writable `T` values.
unsafe fn write_enumeration<T: Copy>(items: &[T], count: *mut u32, data: *mut T) -> VkResult {
    let Some(count) = count.as_mut() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if data.is_null() {
        *count = items.len() as u32;
        return VK_SUCCESS;
    }
    let length = (*count as usize).min(items.len());
    core::ptr::copy_nonoverlapping(items.as_ptr(), data, length);
    *count = length as u32;
    if length < items.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

impl From<&ExtensionProp> for VkExtensionProperties {
    fn from(extension: &ExtensionProp) -> Self {
        let mut properties = Self {
            extension_name: [0; VK_MAX_EXTENSION_NAME_SIZE],
            spec_version: extension.spec_version,
        };
        Name::write(&mut properties.extension_name, extension.name);
        properties
    }
}
