//! Device-group (single-device semantics) + external-handle-property queries + physical-device-group
//! enumeration.
//!
//! hl models exactly ONE physical device and ONE logical device, so the device-group API collapses to a
//! trivially-correct single-device group: peer-memory features are the full local set, a device mask is a
//! no-op, `vkCmdDispatchBase` degenerates to `vkCmdDispatch` (base workgroup 0), and enumeration reports
//! one group of one device. The external-handle-property queries report NO external handle types (this
//! ICD backs no external memory/fence/semaphore sharing), the truthful capability answer.

#![allow(clippy::missing_safety_doc, unused_variables)]

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::StateStore;
use crate::types::{Dispatchable, VkResult, VK_SUCCESS};

struct CommandBuffer;
impl CommandBuffer {
    unsafe fn handle(p: *mut c_void) -> Option<VkCbHandle> {
        Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
    }
}

struct ShimState;
impl ShimState {
    fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        StateStore::with(|s| s.device_mut().map(f))
    }
}

/// `VkPeerMemoryFeatureFlagBits` (stable ABI): COPY_SRC|COPY_DST|GENERIC_SRC|GENERIC_DST.
const PEER_MEMORY_FEATURE_ALL: u32 = 0x1 | 0x2 | 0x4 | 0x8;
/// `VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR`.
const PRESENT_MODE_LOCAL: u32 = 0x1;

// ---- peer memory / device mask / dispatch base -------------------------------------------------

/// `vkGetDeviceGroupPeerMemoryFeatures` — a single device is its own (only) peer, so every peer-memory
/// feature is available. Writes the full feature set.
pub extern "C" fn vkGetDeviceGroupPeerMemoryFeatures(
    _device: *mut c_void,
    _heap_index: u32,
    _local_device_index: u32,
    _remote_device_index: u32,
    p_peer_memory_features: *mut c_void,
) {
    if !p_peer_memory_features.is_null() {
        unsafe { *(p_peer_memory_features as *mut u32) = PEER_MEMORY_FEATURE_ALL };
    }
}
pub extern "C" fn vkGetDeviceGroupPeerMemoryFeaturesKHR(
    device: *mut c_void,
    heap_index: u32,
    local_device_index: u32,
    remote_device_index: u32,
    p_peer_memory_features: *mut c_void,
) {
    vkGetDeviceGroupPeerMemoryFeatures(
        device,
        heap_index,
        local_device_index,
        remote_device_index,
        p_peer_memory_features,
    )
}

/// `vkCmdSetDeviceMask` — a single-device group has exactly one valid mask (bit 0); recording it is a
/// no-op (validate the command buffer is live).
pub extern "C" fn vkCmdSetDeviceMask(command_buffer: *mut c_void, _device_mask: u32) {
    let _ = unsafe { CommandBuffer::handle(command_buffer) };
}
pub extern "C" fn vkCmdSetDeviceMaskKHR(command_buffer: *mut c_void, device_mask: u32) {
    vkCmdSetDeviceMask(command_buffer, device_mask)
}

/// `vkCmdDispatchBase` — with a single device the base workgroup is 0, so this is exactly `vkCmdDispatch`
/// of the group counts. (A non-zero base offset is not modeled by the IR; the count dispatch is recorded.)
pub extern "C" fn vkCmdDispatchBase(
    command_buffer: *mut c_void,
    _base_group_x: u32,
    _base_group_y: u32,
    _base_group_z: u32,
    group_count_x: u32,
    group_count_y: u32,
    group_count_z: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|d| {
        let recorded = record::cmd_dispatch(d, cb, group_count_x, group_count_y, group_count_z);
        d.latch(cb, recorded);
    });
}
pub extern "C" fn vkCmdDispatchBaseKHR(
    command_buffer: *mut c_void,
    base_group_x: u32,
    base_group_y: u32,
    base_group_z: u32,
    group_count_x: u32,
    group_count_y: u32,
    group_count_z: u32,
) {
    vkCmdDispatchBase(
        command_buffer,
        base_group_x,
        base_group_y,
        base_group_z,
        group_count_x,
        group_count_y,
        group_count_z,
    )
}

// ---- device-group present ----------------------------------------------------------------------

const VK_MAX_DEVICE_GROUP_SIZE: usize = 32;

/// `VkDeviceGroupPresentCapabilitiesKHR` (stable layout).
#[repr(C)]
struct VkDeviceGroupPresentCapabilitiesKHR {
    s_type: i32,
    _pad: u32,
    p_next: *mut c_void,
    present_mask: [u32; VK_MAX_DEVICE_GROUP_SIZE],
    modes: u32,
}

pub extern "C" fn vkGetDeviceGroupPresentCapabilitiesKHR(
    _device: *mut c_void,
    p_device_group_present_capabilities: *mut c_void,
) -> VkResult {
    if let Some(caps) = unsafe {
        (p_device_group_present_capabilities as *mut VkDeviceGroupPresentCapabilitiesKHR).as_mut()
    } {
        caps.present_mask = [0; VK_MAX_DEVICE_GROUP_SIZE];
        caps.present_mask[0] = 0x1; // device 0 can present to itself
        caps.modes = PRESENT_MODE_LOCAL;
    }
    VK_SUCCESS
}

/// `vkGetDeviceGroupSurfacePresentModesKHR` — the single device presents locally.
pub extern "C" fn vkGetDeviceGroupSurfacePresentModesKHR(
    _device: *mut c_void,
    _surface: u64,
    p_modes: *mut c_void,
) -> VkResult {
    if !p_modes.is_null() {
        unsafe { *(p_modes as *mut u32) = PRESENT_MODE_LOCAL };
    }
    VK_SUCCESS
}

/// `vkGetDeviceGroupSurfacePresentModes2EXT` (`VK_EXT_full_screen_exclusive`) — same local-present answer.
pub extern "C" fn vkGetDeviceGroupSurfacePresentModes2EXT(
    _device: *mut c_void,
    _p_surface_info: *const c_void,
    p_modes: *mut c_void,
) -> VkResult {
    if !p_modes.is_null() {
        unsafe { *(p_modes as *mut u32) = PRESENT_MODE_LOCAL };
    }
    VK_SUCCESS
}

/// `vkGetPhysicalDevicePresentRectanglesKHR` — one present rectangle for the single device. Follows the
/// two-call enumeration protocol.
pub extern "C" fn vkGetPhysicalDevicePresentRectanglesKHR(
    _physical_device: *mut c_void,
    _surface: u64,
    p_rect_count: *mut c_void,
    p_rects: *mut c_void,
) -> VkResult {
    let count = p_rect_count as *mut u32;
    if count.is_null() {
        return VK_SUCCESS;
    }
    if p_rects.is_null() {
        unsafe { *count = 1 };
        return VK_SUCCESS;
    }
    if unsafe { *count } == 0 {
        return crate::types::VK_INCOMPLETE;
    }
    // One full-surface rectangle (offset 0, extent covers the whole surface — the actual extent is the
    // swapchain's, which the app already knows; a single-device group has exactly this one rect).
    unsafe {
        let r = p_rects as *mut crate::types::VkRect2D;
        (*r).offset.x = 0;
        (*r).offset.y = 0;
        (*r).extent.width = 1;
        (*r).extent.height = 1;
        *count = 1;
    }
    VK_SUCCESS
}

// ---- physical-device-group enumeration ---------------------------------------------------------

/// `VkPhysicalDeviceGroupProperties` (stable layout).
#[repr(C)]
struct VkPhysicalDeviceGroupProperties {
    s_type: i32,
    _pad0: u32,
    p_next: *mut c_void,
    physical_device_count: u32,
    physical_devices: [*mut c_void; VK_MAX_DEVICE_GROUP_SIZE],
    subset_allocation: u32,
}

pub extern "C" fn vkEnumeratePhysicalDeviceGroups(
    _instance: *mut c_void,
    p_physical_device_group_count: *mut c_void,
    p_physical_device_group_properties: *mut c_void,
) -> VkResult {
    let count = p_physical_device_group_count as *mut u32;
    if count.is_null() {
        return VK_SUCCESS;
    }
    if p_physical_device_group_properties.is_null() {
        unsafe { *count = 1 };
        return VK_SUCCESS;
    }
    if unsafe { *count } == 0 {
        return crate::types::VK_INCOMPLETE;
    }
    let phys = StateStore::with(|s| s.phys_dev_handle());
    unsafe {
        let props =
            &mut *(p_physical_device_group_properties as *mut VkPhysicalDeviceGroupProperties);
        props.physical_device_count = 1;
        props.physical_devices[0] = phys;
        props.subset_allocation = 0; // VK_FALSE — a single device is not a subset allocation
        *count = 1;
    }
    VK_SUCCESS
}
pub extern "C" fn vkEnumeratePhysicalDeviceGroupsKHR(
    instance: *mut c_void,
    p_physical_device_group_count: *mut c_void,
    p_physical_device_group_properties: *mut c_void,
) -> VkResult {
    vkEnumeratePhysicalDeviceGroups(
        instance,
        p_physical_device_group_count,
        p_physical_device_group_properties,
    )
}

// ---- external handle properties (report NO external handle types) ------------------------------
// This ICD backs no external memory / fence / semaphore sharing, so every external-property query
// reports zero compatible/exportable handle types + zero features — the truthful capability answer. The
// three property structs share the shape "sType, pNext, then three u32 capability words", so one helper
// zeroes the three words at byte offset 16 (after sType+pad+pNext on LP64).

/// Zero the three `u32` capability words at byte offset 16 of a `Vk*Properties` output struct.
struct ExternalProperties;
impl ExternalProperties {
    unsafe fn zero(p: *mut c_void) {
        if !p.is_null() {
            let words = (p as *mut u8).add(16) as *mut u32;
            *words.add(0) = 0;
            *words.add(1) = 0;
            *words.add(2) = 0;
        }
    }
}

pub extern "C" fn vkGetPhysicalDeviceExternalBufferProperties(
    _physical_device: *mut c_void,
    _p_external_buffer_info: *const c_void,
    p_external_buffer_properties: *mut c_void,
) {
    unsafe { ExternalProperties::zero(p_external_buffer_properties) };
}
pub extern "C" fn vkGetPhysicalDeviceExternalBufferPropertiesKHR(
    physical_device: *mut c_void,
    p_external_buffer_info: *const c_void,
    p_external_buffer_properties: *mut c_void,
) {
    vkGetPhysicalDeviceExternalBufferProperties(
        physical_device,
        p_external_buffer_info,
        p_external_buffer_properties,
    )
}

pub extern "C" fn vkGetPhysicalDeviceExternalFenceProperties(
    _physical_device: *mut c_void,
    _p_external_fence_info: *const c_void,
    p_external_fence_properties: *mut c_void,
) {
    unsafe { ExternalProperties::zero(p_external_fence_properties) };
}
pub extern "C" fn vkGetPhysicalDeviceExternalFencePropertiesKHR(
    physical_device: *mut c_void,
    p_external_fence_info: *const c_void,
    p_external_fence_properties: *mut c_void,
) {
    vkGetPhysicalDeviceExternalFenceProperties(
        physical_device,
        p_external_fence_info,
        p_external_fence_properties,
    )
}

pub extern "C" fn vkGetPhysicalDeviceExternalSemaphoreProperties(
    _physical_device: *mut c_void,
    _p_external_semaphore_info: *const c_void,
    p_external_semaphore_properties: *mut c_void,
) {
    unsafe { ExternalProperties::zero(p_external_semaphore_properties) };
}
pub extern "C" fn vkGetPhysicalDeviceExternalSemaphorePropertiesKHR(
    physical_device: *mut c_void,
    p_external_semaphore_info: *const c_void,
    p_external_semaphore_properties: *mut c_void,
) {
    vkGetPhysicalDeviceExternalSemaphoreProperties(
        physical_device,
        p_external_semaphore_info,
        p_external_semaphore_properties,
    )
}
