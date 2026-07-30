//! Debug-only entry points: `VK_EXT_debug_utils`, `VK_EXT_debug_marker`, and `VK_EXT_debug_report`.
//!
//! None of these extensions is advertised (advertise-only-what's-real), but every entry point succeeds
//! benignly: they are debug/diagnostic hooks with no rendering effect, so honoring them as no-ops is
//! safe and correct (an app that resolved the symbol gets the success it expects, and object names are
//! actually stored so a later trace can surface them). Object-name/tag setters record the name; label
//! commands are no-ops; messenger/callback create mints a real live handle and destroy reclaims it;
//! message-submission is a no-op. Nothing here ever fakes a rendering capability.

#![allow(clippy::missing_safety_doc, unused_variables)]

use core::ffi::{c_char, c_void};

use crate::state::StateStore;
use crate::types::{VkResult, VK_SUCCESS};

/// `VkDebugUtilsObjectNameInfoEXT` head (LP64): `sType`(+pad), `pNext`, `objectType` (i32, +pad),
/// `objectHandle` (u64), `pObjectName` (char*). Also matches `VkDebugMarkerObjectNameInfoEXT` field order.
#[repr(C)]
struct VkDebugUtilsObjectNameInfoEXT {
    s_type: i32,
    _pad0: u32,
    p_next: *const c_void,
    object_type: i32,
    _pad1: u32,
    object_handle: u64,
    p_object_name: *const c_char,
}

/// `VkDebugMarkerObjectNameInfoEXT` head — `objectType` is a `VkDebugReportObjectTypeEXT`, `object` the
/// handle; same offsets as above.
#[repr(C)]
struct VkDebugMarkerObjectNameInfoEXT {
    s_type: i32,
    _pad0: u32,
    p_next: *const c_void,
    object_type: i32,
    _pad1: u32,
    object: u64,
    p_object_name: *const c_char,
}

/// Borrow a nul-terminated C string as an owned `String` (empty on NULL / bad UTF-8).
struct DebugName;
impl DebugName {
    unsafe fn read(p: *const c_char) -> String {
        if p.is_null() {
            return String::new();
        }
        core::ffi::CStr::from_ptr(p)
            .to_str()
            .unwrap_or("")
            .to_string()
    }
}

// ==================================================================================================
// VK_EXT_debug_utils
// ==================================================================================================

pub extern "C" fn vkSetDebugUtilsObjectNameEXT(
    _device: *mut c_void,
    p_name_info: *const c_void,
) -> VkResult {
    if let Some(info) = unsafe { (p_name_info as *const VkDebugUtilsObjectNameInfoEXT).as_ref() } {
        let name = unsafe { DebugName::read(info.p_object_name) };
        StateStore::with(|s| {
            if name.is_empty() {
                s.debug_object_names
                    .remove(&(info.object_type, info.object_handle));
            } else {
                s.debug_object_names
                    .insert((info.object_type, info.object_handle), name);
            }
        });
    }
    VK_SUCCESS
}

pub extern "C" fn vkSetDebugUtilsObjectTagEXT(
    _device: *mut c_void,
    _p_tag_info: *const c_void,
) -> VkResult {
    // Tags are opaque app data with no modeled effect; accepting them is a benign no-op.
    VK_SUCCESS
}

pub extern "C" fn vkQueueBeginDebugUtilsLabelEXT(
    _queue: *mut c_void,
    _p_label_info: *const c_void,
) {
}
pub extern "C" fn vkQueueEndDebugUtilsLabelEXT(_queue: *mut c_void) {}
pub extern "C" fn vkQueueInsertDebugUtilsLabelEXT(
    _queue: *mut c_void,
    _p_label_info: *const c_void,
) {
}
pub extern "C" fn vkCmdBeginDebugUtilsLabelEXT(
    _command_buffer: *mut c_void,
    _p_label_info: *const c_void,
) {
}
pub extern "C" fn vkCmdEndDebugUtilsLabelEXT(_command_buffer: *mut c_void) {}
pub extern "C" fn vkCmdInsertDebugUtilsLabelEXT(
    _command_buffer: *mut c_void,
    _p_label_info: *const c_void,
) {
}

pub extern "C" fn vkCreateDebugUtilsMessengerEXT(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_messenger: *mut c_void,
) -> VkResult {
    if p_messenger.is_null() {
        return VK_SUCCESS;
    }
    let handle = StateStore::with(|s| {
        let h = s.mint_aux();
        s.debug_messengers.insert(h);
        h
    });
    unsafe { *(p_messenger as *mut u64) = handle };
    VK_SUCCESS
}

pub extern "C" fn vkDestroyDebugUtilsMessengerEXT(
    _instance: *mut c_void,
    messenger: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.debug_messengers.remove(&messenger);
    });
}

pub extern "C" fn vkSubmitDebugUtilsMessageEXT(
    _instance: *mut c_void,
    _message_severity: i32,
    _message_types: u32,
    _p_callback_data: *const c_void,
) {
}

// ==================================================================================================
// VK_EXT_debug_marker (device object naming/tagging + command-buffer markers)
// ==================================================================================================

pub extern "C" fn vkDebugMarkerSetObjectNameEXT(
    _device: *mut c_void,
    p_name_info: *const c_void,
) -> VkResult {
    if let Some(info) = unsafe { (p_name_info as *const VkDebugMarkerObjectNameInfoEXT).as_ref() } {
        let name = unsafe { DebugName::read(info.p_object_name) };
        StateStore::with(|s| {
            if name.is_empty() {
                s.debug_object_names
                    .remove(&(info.object_type, info.object));
            } else {
                s.debug_object_names
                    .insert((info.object_type, info.object), name);
            }
        });
    }
    VK_SUCCESS
}

pub extern "C" fn vkDebugMarkerSetObjectTagEXT(
    _device: *mut c_void,
    _p_tag_info: *const c_void,
) -> VkResult {
    VK_SUCCESS
}

pub extern "C" fn vkCmdDebugMarkerBeginEXT(
    _command_buffer: *mut c_void,
    _p_marker_info: *const c_void,
) {
}
pub extern "C" fn vkCmdDebugMarkerEndEXT(_command_buffer: *mut c_void) {}
pub extern "C" fn vkCmdDebugMarkerInsertEXT(
    _command_buffer: *mut c_void,
    _p_marker_info: *const c_void,
) {
}

// ==================================================================================================
// VK_EXT_debug_report (deprecated instance-level callback)
// ==================================================================================================

pub extern "C" fn vkCreateDebugReportCallbackEXT(
    _instance: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_callback: *mut c_void,
) -> VkResult {
    if p_callback.is_null() {
        return VK_SUCCESS;
    }
    let handle = StateStore::with(|s| {
        let h = s.mint_aux();
        s.debug_report_callbacks.insert(h);
        h
    });
    unsafe { *(p_callback as *mut u64) = handle };
    VK_SUCCESS
}

pub extern "C" fn vkDestroyDebugReportCallbackEXT(
    _instance: *mut c_void,
    callback: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.debug_report_callbacks.remove(&callback);
    });
}

pub extern "C" fn vkDebugReportMessageEXT(
    _instance: *mut c_void,
    _flags: u32,
    _object_type: i32,
    _object: u64,
    _location: usize,
    _message_code: i32,
    _p_layer_prefix: *const c_char,
    _p_message: *const c_char,
) {
}
