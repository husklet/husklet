//! Small CORE (`VK_VERSION_1_x`) entry points with real, benign bodies that the generated truthful stub
//! would get wrong (a core command that an app legitimately calls must NOT return a false error).
//!
//! * host-visible memory is unified + coherent, so `vkFlush/InvalidateMappedMemoryRanges` are true no-op
//!   successes and `vkGetDeviceMemoryCommitment` reports the allocation fully committed;
//! * render-area / rendering-area granularity is `(1,1)` (no tile constraint in the software backend);
//! * `vkCreate/DestroyBufferView` mint/reclaim a real host-object handle;
//! * `vkFreeDescriptorSets` / `vkResetDescriptorPool` actually drop the modeled sets;
//! * sparse-image-format + tool + multisample property queries report the truthful empty/unsupported set;
//! * `vkCmdResolveImage*` lower to an image COPY (hl images are single-sample, so a resolve MOVES the
//!   rendered content into the resolve target — see [`record::cmd_resolve_image`]);
//! * `vkCmdDraw*IndirectCount*` read the actual draw count from the host-visible count buffer and lower
//!   to direct draws (see [`record::cmd_draw_indirect_count`]);
//! * `vkCmdClearDepthStencilImage` lowers to a zero-draw depth-clear render pass — the executor clears a
//!   depth attachment on a CLEAR loadOp, so a standalone depth clear reuses that (see
//!   [`record::cmd_clear_depth_stencil_image`]).

#![allow(
    clippy::missing_safety_doc,
    unused_variables,
    clippy::too_many_arguments
)]

use core::ffi::c_void;

use hl_gpu::CommandSink;
use hl_vulkan::service::create;
use hl_vulkan::{model::memory::Format, result::Status, Device};

use crate::state::StateStore;
use crate::types::{
    VkBufferViewCreateInfo, VkExtent2D, VkMappedMemoryRange, VkResult, VK_ERROR_INITIALIZATION_FAILED,
    VK_SUCCESS,
};

#[path = "corebits_command.rs"]
mod command;
pub use command::*;

struct ShimState;
impl ShimState {
    fn with_device<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        StateStore::with(|s| s.device_mut().map(f))
    }

    /// Run `f` with the logical device + the command sink (disjoint `State` fields) — the readback path
    /// `vkInvalidateMappedMemoryRanges` needs. `None` if no device exists yet.
    fn with_sink<R>(f: impl FnOnce(&mut Device, &mut dyn CommandSink) -> R) -> Option<R> {
        StateStore::with(|s| {
            let (dev, sink) = s.device_and_sink()?;
            Some(f(dev, sink))
        })
    }
}

// ---- mapped-memory flush/invalidate (unified coherent memory → no-op success) ------------------

pub extern "C" fn vkFlushMappedMemoryRanges(
    _device: *mut c_void,
    memory_range_count: u32,
    p_memory_ranges: *const c_void,
) -> VkResult {
    // The model's device memory is host-visible + HOST_COHERENT, so no host-side flush is needed. But the
    // host→device DIRECTION still matters: an app that stages into a mapped buffer, flushes, then UNMAPs
    // before submitting must not lose the write. Capture each flushed range as a pending upload keyed by
    // its memory (honoring offset/size); the next vkQueueSubmit ships it as a Cmd::WriteBuffer even if the
    // memory has since been unmapped. Bound buffers only (unbound host-only staging is the true no-op the
    // coherent contract promises); coalesced with the still-mapped flush so nothing is written twice.
    if !p_memory_ranges.is_null() && memory_range_count != 0 {
        let ranges = unsafe {
            core::slice::from_raw_parts(
                p_memory_ranges as *const VkMappedMemoryRange,
                memory_range_count as usize,
            )
        };
        ShimState::with_device(|d| {
            for r in ranges {
                create::capture_pending_upload(d, r.memory, r.offset, r.size);
            }
        });
    }
    VK_SUCCESS
}

/// `vkInvalidateMappedMemoryRanges` — the spec-correct point where the host must see the device's writes
/// to a non-coherent mapped allocation. The model's memory is coherent, but a defensive app that maps a
/// buffer and then invalidates before reading must still observe GPU output. So each range's staging bytes
/// are refreshed with the bound buffer's CURRENT device contents via the device→host readback (the same
/// path `vkMapMemory` and cuda's `cuMemcpyDtoH` use). Ranges over host-only (unbound) staging are the true
/// no-op the coherent-memory contract promises.
pub extern "C" fn vkInvalidateMappedMemoryRanges(
    _device: *mut c_void,
    memory_range_count: u32,
    p_memory_ranges: *const c_void,
) -> VkResult {
    if p_memory_ranges.is_null() || memory_range_count == 0 {
        return VK_SUCCESS;
    }
    let ranges = unsafe {
        core::slice::from_raw_parts(
            p_memory_ranges as *const VkMappedMemoryRange,
            memory_range_count as usize,
        )
    };
    ShimState::with_sink(|dev, sink| {
        for r in ranges {
            // A readback transport error on one range is non-fatal to the invalidate; the memory stays
            // valid host-visible staging.
            let _ = create::read_mapped(dev, sink, r.memory, r.offset, r.size);
        }
    });
    VK_SUCCESS
}

/// `vkGetDeviceMemoryCommitment` — non-lazy allocations are fully committed, so report the allocation
/// size (0 for an unknown handle).
pub extern "C" fn vkGetDeviceMemoryCommitment(
    _device: *mut c_void,
    memory: u64,
    p_committed_memory_in_bytes: *mut c_void,
) {
    if p_committed_memory_in_bytes.is_null() {
        return;
    }
    let size = ShimState::with_device(|d| d.memories.get(&memory).map(|m| m.size).unwrap_or(0))
        .unwrap_or(0);
    unsafe { *(p_committed_memory_in_bytes as *mut u64) = size };
}

// ---- render-area / rendering-area granularity (no tiling constraint → (1,1)) -------------------

pub extern "C" fn vkGetRenderAreaGranularity(
    _device: *mut c_void,
    _render_pass: u64,
    p_granularity: *mut c_void,
) {
    if !p_granularity.is_null() {
        unsafe {
            *(p_granularity as *mut VkExtent2D) = VkExtent2D {
                width: 1,
                height: 1,
            }
        };
    }
}

pub extern "C" fn vkGetRenderingAreaGranularity(
    _device: *mut c_void,
    _p_rendering_area_info: *const c_void,
    p_granularity: *mut c_void,
) {
    if !p_granularity.is_null() {
        unsafe {
            *(p_granularity as *mut VkExtent2D) = VkExtent2D {
                width: 1,
                height: 1,
            }
        };
    }
}
pub extern "C" fn vkGetRenderingAreaGranularityKHR(
    device: *mut c_void,
    p_rendering_area_info: *const c_void,
    p_granularity: *mut c_void,
) {
    vkGetRenderingAreaGranularity(device, p_rendering_area_info, p_granularity)
}

/// `vkGetDeviceImageSubresourceLayout(KHR)` — the software backend exposes no linear tiling layout, so
/// the `VkSubresourceLayout` (five `VkDeviceSize` at byte offset 16 of the `VkSubresourceLayout2` output)
/// is reported as all-zero (offset/size/pitches). Honest for a model with no addressable image bytes.
pub extern "C" fn vkGetDeviceImageSubresourceLayout(
    _device: *mut c_void,
    _p_info: *const c_void,
    p_layout: *mut c_void,
) {
    if !p_layout.is_null() {
        unsafe {
            let words = (p_layout as *mut u8).add(16) as *mut u64;
            for i in 0..5 {
                *words.add(i) = 0;
            }
        }
    }
}
pub extern "C" fn vkGetDeviceImageSubresourceLayoutKHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_layout: *mut c_void,
) {
    vkGetDeviceImageSubresourceLayout(device, p_info, p_layout)
}

// ---- buffer views (pure host objects) ----------------------------------------------------------

pub extern "C" fn vkCreateBufferView(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_view: *mut c_void,
) -> VkResult {
    if p_view.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    unsafe { *(p_view as *mut u64) = 0 };
    let Some(ci) = (unsafe { (p_create_info as *const VkBufferViewCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some(format) = Format(ci.format as u32).wire() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    match ShimState::with_device(|dev| {
        create::create_buffer_view(dev, ci.buffer, format, ci.offset, ci.range)
    }) {
        Some(Ok(handle)) => {
            unsafe { *(p_view as *mut u64) = handle };
            VK_SUCCESS
        }
        Some(Err(error)) => Status::from_error(&error),
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

pub extern "C" fn vkDestroyBufferView(
    _device: *mut c_void,
    buffer_view: u64,
    _p_allocator: *const c_void,
) {
    let _ = ShimState::with_device(|dev| create::destroy_buffer_view(dev, buffer_view));
}

// ---- descriptor pool free / reset --------------------------------------------------------------

pub extern "C" fn vkFreeDescriptorSets(
    _device: *mut c_void,
    _descriptor_pool: u64,
    descriptor_set_count: u32,
    p_descriptor_sets: *const c_void,
) -> VkResult {
    if !p_descriptor_sets.is_null() && descriptor_set_count > 0 {
        let sets = unsafe {
            core::slice::from_raw_parts(
                p_descriptor_sets as *const u64,
                descriptor_set_count as usize,
            )
        };
        ShimState::with_device(|d| {
            for &s in sets {
                d.descriptor_sets.remove(&s);
            }
        });
    }
    VK_SUCCESS
}

pub extern "C" fn vkResetDescriptorPool(
    _device: *mut c_void,
    descriptor_pool: u64,
    _flags: u32,
) -> VkResult {
    ShimState::with_device(|d| {
        d.descriptor_sets.retain(|_, r| r.pool != descriptor_pool);
    });
    VK_SUCCESS
}

// ---- sparse image format properties (no sparse support → empty) --------------------------------

pub extern "C" fn vkGetPhysicalDeviceSparseImageFormatProperties(
    _physical_device: *mut c_void,
    _format: i32,
    _type_: i32,
    _samples: i32,
    _usage: u32,
    _tiling: i32,
    p_property_count: *mut c_void,
    _p_properties: *mut c_void,
) {
    if !p_property_count.is_null() {
        unsafe { *(p_property_count as *mut u32) = 0 };
    }
}

pub extern "C" fn vkGetPhysicalDeviceSparseImageFormatProperties2(
    _physical_device: *mut c_void,
    _p_format_info: *const c_void,
    p_property_count: *mut c_void,
    _p_properties: *mut c_void,
) {
    if !p_property_count.is_null() {
        unsafe { *(p_property_count as *mut u32) = 0 };
    }
}
pub extern "C" fn vkGetPhysicalDeviceSparseImageFormatProperties2KHR(
    physical_device: *mut c_void,
    p_format_info: *const c_void,
    p_property_count: *mut c_void,
    p_properties: *mut c_void,
) {
    vkGetPhysicalDeviceSparseImageFormatProperties2(
        physical_device,
        p_format_info,
        p_property_count,
        p_properties,
    )
}

// ---- tool properties (no active tools) ---------------------------------------------------------

pub extern "C" fn vkGetPhysicalDeviceToolProperties(
    _physical_device: *mut c_void,
    p_tool_count: *mut c_void,
    _p_tool_properties: *mut c_void,
) -> VkResult {
    if !p_tool_count.is_null() {
        unsafe { *(p_tool_count as *mut u32) = 0 };
    }
    VK_SUCCESS
}
pub extern "C" fn vkGetPhysicalDeviceToolPropertiesEXT(
    physical_device: *mut c_void,
    p_tool_count: *mut c_void,
    p_tool_properties: *mut c_void,
) -> VkResult {
    vkGetPhysicalDeviceToolProperties(physical_device, p_tool_count, p_tool_properties)
}

/// `vkGetPhysicalDeviceMultisamplePropertiesEXT` — no programmable sample locations; report a
/// `maxSampleLocationGridSize` of `(0,0)` (the "unsupported" answer) into the `VkMultisamplePropertiesEXT`
/// (a `VkExtent2D` at byte offset 16).
pub extern "C" fn vkGetPhysicalDeviceMultisamplePropertiesEXT(
    _physical_device: *mut c_void,
    _samples: i32,
    p_multisample_properties: *mut c_void,
) {
    if !p_multisample_properties.is_null() {
        unsafe {
            let ext = (p_multisample_properties as *mut u8).add(16) as *mut VkExtent2D;
            *ext = VkExtent2D {
                width: 0,
                height: 0,
            };
        }
    }
}
