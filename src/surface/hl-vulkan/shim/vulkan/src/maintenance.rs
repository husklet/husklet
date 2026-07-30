//! Modeled maintenance1-5, private-data, ycbcr-conversion, and calibrated-timestamp entry points.
//! Each operation mutates or queries real shim state; unsupported sparse residency is reported as empty.

use core::ffi::c_void;

use crate::state::StateStore;
use crate::types::*;
use hl_vulkan::model::command::CommandBufferState;
use hl_vulkan::model::memory::Format;

#[path = "maintenance_support.rs"]
mod support;

use support::{CommandBuffer, MemoryRequirements, ShimState, SparseRequirements};

/// `vkTrimCommandPool` — recycle a pool's unused command-buffer memory. The bring-up model pools no
/// backing memory (command buffers are plain device-table records), so trimming is a truthful no-op.
#[no_mangle]
pub extern "C" fn vkTrimCommandPool(_device: *mut c_void, _command_pool: u64, _flags: u32) {}

/// `vkResetCommandPool` — reset every command buffer the device owns back to `Initial` (the model does
/// not scope buffers to a pool object, so a pool reset resets all — a superset that is spec-safe for the
/// single-pool bring-up flow). Clears each recording.
#[no_mangle]
pub extern "C" fn vkResetCommandPool(
    _device: *mut c_void,
    _command_pool: u64,
    _flags: u32,
) -> VkResult {
    ShimState::with_device_result(|d| {
        for rec in d.command_buffers.values_mut() {
            rec.reset_recording();
            rec.state = CommandBufferState::Initial;
        }
        VK_SUCCESS
    })
}

/// `vkResetCommandBuffer` — reset one command buffer to `Initial`, clearing its recording. Errors on an
/// unknown handle.
#[no_mangle]
pub extern "C" fn vkResetCommandBuffer(command_buffer: *mut c_void, _flags: u32) -> VkResult {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    ShimState::with_device_result(|d| match d.command_buffers.get_mut(&cb) {
        Some(rec) => {
            rec.reset_recording();
            rec.state = CommandBufferState::Initial;
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    })
}

/// `vkFreeCommandBuffers` — drop each command buffer's device-table record and reclaim its dispatchable
/// box. A null slot / unknown handle is skipped (spec: freeing `VK_NULL_HANDLE` is valid).
#[no_mangle]
pub extern "C" fn vkFreeCommandBuffers(
    _device: *mut c_void,
    _command_pool: u64,
    command_buffer_count: u32,
    p_command_buffers: *const *mut c_void,
) {
    if p_command_buffers.is_null() || command_buffer_count == 0 {
        return;
    }
    let raw =
        unsafe { std::slice::from_raw_parts(p_command_buffers, command_buffer_count as usize) };
    for &p in raw {
        if p.is_null() {
            continue;
        }
        if let Some(h) = unsafe { CommandBuffer::handle(p) } {
            StateStore::with(|s| {
                if let Some(d) = s.device.as_mut() {
                    d.command_buffers.remove(&h);
                }
            });
        }
        unsafe { Dispatchable::<u64>::free(p) };
    }
}

/// `vkGetDescriptorSetLayoutSupport(KHR)` — report whether a set of the queried layout can be created.
/// The bring-up model accepts any layout within the reported device limits, so `supported = VK_TRUE`.
#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutSupport(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    p_support: *mut c_void,
) {
    if let Some(out) = unsafe { (p_support as *mut VkDescriptorSetLayoutSupport).as_mut() } {
        out.supported = VK_TRUE;
    }
}

/// `vkGetDescriptorSetLayoutSupportKHR` — the `VK_KHR_maintenance3` alias.
#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutSupportKHR(
    device: *mut c_void,
    p_create_info: *const c_void,
    p_support: *mut c_void,
) {
    vkGetDescriptorSetLayoutSupport(device, p_create_info, p_support)
}

/// `vkGetDeviceBufferMemoryRequirements(KHR)` — derive a buffer's requirements from its create info
/// WITHOUT creating it: the size is the requested `VkBufferCreateInfo::size`, aligned like a real buffer.
#[no_mangle]
pub extern "C" fn vkGetDeviceBufferMemoryRequirements(
    _device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    let size = unsafe {
        (p_info as *const VkDeviceBufferMemoryRequirements)
            .as_ref()
            .and_then(|i| i.p_create_info.as_ref())
            .map(|ci| ci.size)
            .unwrap_or(0)
    };
    MemoryRequirements::write(p_memory_requirements, size);
}

/// `vkGetDeviceBufferMemoryRequirementsKHR` — the `VK_KHR_maintenance4` alias.
#[no_mangle]
pub extern "C" fn vkGetDeviceBufferMemoryRequirementsKHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    vkGetDeviceBufferMemoryRequirements(device, p_info, p_memory_requirements)
}

/// `vkGetDeviceImageMemoryRequirements(KHR)` — derive an image's requirements from its create info:
/// `size = width*height*bytes_per_texel(format)`. The size MUST be format-aware — a blind *4 over-reports a
/// 1-byte R8 coverage atlas 4x, and once GPUI grows its glyph atlas that inflated requirement crosses
/// gpu-alloc's 2 GiB max-allocation ceiling and spuriously OutOfMemory-device-losts wgpu (see the sibling
/// `vkGetImageMemoryRequirements`).
#[no_mangle]
pub extern "C" fn vkGetDeviceImageMemoryRequirements(
    _device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    let size = unsafe {
        (p_info as *const VkDeviceImageMemoryRequirements)
            .as_ref()
            .and_then(|i| i.p_create_info.as_ref())
            .and_then(|ci| {
                let bpt = Format(ci.format as u32).wire()?.bytes_per_texel()? as u64;
                Some(ci.extent.width as u64 * ci.extent.height.max(1) as u64 * bpt)
            })
            .unwrap_or(0)
    };
    MemoryRequirements::write(p_memory_requirements, size);
}

/// `vkGetDeviceImageMemoryRequirementsKHR` — the `VK_KHR_maintenance4` alias.
#[no_mangle]
pub extern "C" fn vkGetDeviceImageMemoryRequirementsKHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    vkGetDeviceImageMemoryRequirements(device, p_info, p_memory_requirements)
}

#[no_mangle]
pub extern "C" fn vkGetImageSparseMemoryRequirements(
    _device: *mut c_void,
    _image: u64,
    p_sparse_memory_requirement_count: *mut u32,
    _p_sparse_memory_requirements: *mut c_void,
) {
    SparseRequirements::write_empty(p_sparse_memory_requirement_count);
}

#[no_mangle]
pub extern "C" fn vkGetImageSparseMemoryRequirements2(
    _device: *mut c_void,
    _p_info: *const c_void,
    p_sparse_memory_requirement_count: *mut u32,
    _p_sparse_memory_requirements: *mut c_void,
) {
    SparseRequirements::write_empty(p_sparse_memory_requirement_count);
}

#[no_mangle]
pub extern "C" fn vkGetImageSparseMemoryRequirements2KHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_sparse_memory_requirement_count: *mut u32,
    p_sparse_memory_requirements: *mut c_void,
) {
    vkGetImageSparseMemoryRequirements2(
        device,
        p_info,
        p_sparse_memory_requirement_count,
        p_sparse_memory_requirements,
    )
}

#[no_mangle]
pub extern "C" fn vkGetDeviceImageSparseMemoryRequirements(
    _device: *mut c_void,
    _p_info: *const c_void,
    p_sparse_memory_requirement_count: *mut u32,
    _p_sparse_memory_requirements: *mut c_void,
) {
    SparseRequirements::write_empty(p_sparse_memory_requirement_count);
}

#[no_mangle]
pub extern "C" fn vkGetDeviceImageSparseMemoryRequirementsKHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_sparse_memory_requirement_count: *mut u32,
    p_sparse_memory_requirements: *mut c_void,
) {
    vkGetDeviceImageSparseMemoryRequirements(
        device,
        p_info,
        p_sparse_memory_requirement_count,
        p_sparse_memory_requirements,
    )
}

// private data (VK_EXT_private_data / core 1.3) — a real per-object `u64` store

#[no_mangle]
pub extern "C" fn vkCreatePrivateDataSlot(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_private_data_slot: *mut u64,
) -> VkResult {
    if !p_private_data_slot.is_null() {
        unsafe { *p_private_data_slot = 0 };
    }
    StateStore::with(|s| {
        let Some(d) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let h = d.alloc_handle();
        s.private_data_slots.insert(h);
        if !p_private_data_slot.is_null() {
            unsafe { *p_private_data_slot = h };
        }
        VK_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn vkCreatePrivateDataSlotEXT(
    device: *mut c_void,
    p_create_info: *const c_void,
    p_allocator: *const c_void,
    p_private_data_slot: *mut u64,
) -> VkResult {
    vkCreatePrivateDataSlot(device, p_create_info, p_allocator, p_private_data_slot)
}

#[no_mangle]
pub extern "C" fn vkDestroyPrivateDataSlot(
    _device: *mut c_void,
    private_data_slot: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.private_data_slots.remove(&private_data_slot);
        s.private_data
            .retain(|(_, _, slot), _| *slot != private_data_slot);
    });
}

#[no_mangle]
pub extern "C" fn vkDestroyPrivateDataSlotEXT(
    device: *mut c_void,
    private_data_slot: u64,
    p_allocator: *const c_void,
) {
    vkDestroyPrivateDataSlot(device, private_data_slot, p_allocator)
}

#[no_mangle]
pub extern "C" fn vkSetPrivateData(
    _device: *mut c_void,
    object_type: i32,
    object_handle: u64,
    private_data_slot: u64,
    data: u64,
) -> VkResult {
    StateStore::with(|s| {
        if !s.private_data_slots.contains(&private_data_slot) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        s.private_data
            .insert((object_type, object_handle, private_data_slot), data);
        VK_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn vkSetPrivateDataEXT(
    device: *mut c_void,
    object_type: i32,
    object_handle: u64,
    private_data_slot: u64,
    data: u64,
) -> VkResult {
    vkSetPrivateData(device, object_type, object_handle, private_data_slot, data)
}

#[no_mangle]
pub extern "C" fn vkGetPrivateData(
    _device: *mut c_void,
    object_type: i32,
    object_handle: u64,
    private_data_slot: u64,
    p_data: *mut u64,
) {
    let v = StateStore::with(|s| {
        s.private_data
            .get(&(object_type, object_handle, private_data_slot))
            .copied()
            .unwrap_or(0)
    });
    if let Some(out) = unsafe { p_data.as_mut() } {
        *out = v;
    }
}

#[no_mangle]
pub extern "C" fn vkGetPrivateDataEXT(
    device: *mut c_void,
    object_type: i32,
    object_handle: u64,
    private_data_slot: u64,
    p_data: *mut u64,
) {
    vkGetPrivateData(
        device,
        object_type,
        object_handle,
        private_data_slot,
        p_data,
    )
}

// sampler ycbcr conversion (VK_KHR_sampler_ycbcr_conversion / core 1.1) — a real host object

#[no_mangle]
pub extern "C" fn vkCreateSamplerYcbcrConversion(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_ycbcr_conversion: *mut u64,
) -> VkResult {
    if !p_ycbcr_conversion.is_null() {
        unsafe { *p_ycbcr_conversion = 0 };
    }
    if p_create_info.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    StateStore::with(|s| {
        let Some(d) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let h = d.alloc_handle();
        s.ycbcr_conversions.insert(h);
        if !p_ycbcr_conversion.is_null() {
            unsafe { *p_ycbcr_conversion = h };
        }
        VK_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn vkCreateSamplerYcbcrConversionKHR(
    device: *mut c_void,
    p_create_info: *const c_void,
    p_allocator: *const c_void,
    p_ycbcr_conversion: *mut u64,
) -> VkResult {
    vkCreateSamplerYcbcrConversion(device, p_create_info, p_allocator, p_ycbcr_conversion)
}

#[no_mangle]
pub extern "C" fn vkDestroySamplerYcbcrConversion(
    _device: *mut c_void,
    ycbcr_conversion: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.ycbcr_conversions.remove(&ycbcr_conversion);
    });
}

#[no_mangle]
pub extern "C" fn vkDestroySamplerYcbcrConversionKHR(
    device: *mut c_void,
    ycbcr_conversion: u64,
    p_allocator: *const c_void,
) {
    vkDestroySamplerYcbcrConversion(device, ycbcr_conversion, p_allocator)
}

// calibrated timestamps (VK_KHR_calibrated_timestamps) — monotonic device serials

/// `vkGetCalibratedTimestampsKHR` — write one host-monotonic serial per queried timestamp info and a
/// zero max-deviation (the synchronous model has no sampling jitter). Errors on a null info/output array.
#[no_mangle]
pub extern "C" fn vkGetCalibratedTimestampsKHR(
    _device: *mut c_void,
    timestamp_count: u32,
    p_timestamp_infos: *const c_void,
    p_timestamps: *mut u64,
    p_max_deviation: *mut u64,
) -> VkResult {
    if p_timestamp_infos.is_null() || p_timestamps.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let n = timestamp_count as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(p_timestamps, n) };
    ShimState::with_device_result(|d| {
        for slot in out.iter_mut() {
            *slot = d.next_timestamp();
        }
        if let Some(dev_out) = unsafe { p_max_deviation.as_mut() } {
            *dev_out = 0;
        }
        VK_SUCCESS
    })
}

/// `vkGetCalibratedTimestampsEXT` — the `VK_EXT_calibrated_timestamps` alias.
#[no_mangle]
pub extern "C" fn vkGetCalibratedTimestampsEXT(
    device: *mut c_void,
    timestamp_count: u32,
    p_timestamp_infos: *const c_void,
    p_timestamps: *mut u64,
    p_max_deviation: *mut u64,
) -> VkResult {
    vkGetCalibratedTimestampsKHR(
        device,
        timestamp_count,
        p_timestamp_infos,
        p_timestamps,
        p_max_deviation,
    )
}

/// `vkGetPhysicalDeviceCalibrateableTimeDomainsKHR` — the modeled device reports one calibrateable
/// domain (`VK_TIME_DOMAIN_DEVICE_KHR`), via the standard two-call enumeration.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceCalibrateableTimeDomainsKHR(
    _physical_device: *mut c_void,
    p_time_domain_count: *mut u32,
    p_time_domains: *mut i32,
) -> VkResult {
    let Some(count) = (unsafe { p_time_domain_count.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let domains = [VK_TIME_DOMAIN_DEVICE_KHR];
    if p_time_domains.is_null() {
        *count = domains.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(domains.len());
    let out = unsafe { std::slice::from_raw_parts_mut(p_time_domains, n) };
    out.copy_from_slice(&domains[..n]);
    *count = n as u32;
    if n < domains.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

/// `vkGetPhysicalDeviceCalibrateableTimeDomainsEXT` — the `VK_EXT_calibrated_timestamps` alias.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceCalibrateableTimeDomainsEXT(
    physical_device: *mut c_void,
    p_time_domain_count: *mut u32,
    p_time_domains: *mut i32,
) -> VkResult {
    vkGetPhysicalDeviceCalibrateableTimeDomainsKHR(
        physical_device,
        p_time_domain_count,
        p_time_domains,
    )
}
