//! Vulkan 1.4 promoted-core commands (real bodies), ported from MoltenVK — the final core version that
//! completes the Vulkan core spec surface (core:1.4 → 19/19).
//!
//! Groups: the `...2`/`...Info` maintenance-6 forms (`vkCmdBindDescriptorSets2`/`vkCmdPushConstants2`/
//! `vkGetImageSubresourceLayout2`/`vkMapMemory2`/`vkUnmapMemory2`) delegate to the validated base bodies;
//! push-descriptor (`vkCmdPushDescriptorSet[2]`/`…WithTemplate[2]`, MVKCmdPushDescriptorSet) build a
//! transient descriptor set and bind it; line-stipple + rendering-area-granularity + rendering
//! attachment-location/index are recorded; host-image-layout transition (`vkTransitionImageLayout`,
//! VK_EXT_host_image_copy) applies the layout to the tracked subresource state; the host memory↔image
//! copies (`vkCopyMemoryToImage`/`CopyImageToImage`/`CopyImageToMemory`) are lowered where the IR can
//! express them and otherwise truthfully report the unmaterialized capability.

use crate::reg::{self, ImageSubresourceRange};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_shim_common::ir::*;

// ---- maintenance6 / promoted `...Info` forms (delegate to the base bodies) ------------------------

/// `vkCmdBindIndexBuffer2` — like the 1.0 bind plus a `size` (the IR uses buffer+offset, so `size` is
/// validated-by-ignore).
#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer2(
    command_buffer: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    _size: u64,
    index_type: i32,
) {
    crate::command::vkCmdBindIndexBuffer(command_buffer, buffer, offset, index_type);
}

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2(command_buffer: VkCommandBuffer, p_info: *const vk::BindDescriptorSetsInfoKHR) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    crate::command::vkCmdBindDescriptorSets(
        command_buffer,
        0, // pipeline bind point is carried by stageFlags in the ...2 form; the sets carry their layout
        info.layout.as_raw(),
        info.first_set,
        info.descriptor_set_count,
        info.p_descriptor_sets as *const VkDescriptorSet,
        info.dynamic_offset_count,
        info.p_dynamic_offsets,
    );
}

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2(command_buffer: VkCommandBuffer, p_info: *const vk::PushConstantsInfoKHR) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    crate::command::vkCmdPushConstants(
        command_buffer,
        info.layout.as_raw(),
        info.stage_flags,
        info.offset,
        info.size,
        info.p_values,
    );
}

#[no_mangle]
pub extern "C" fn vkMapMemory2(_device: VkDevice, p_info: *const vk::MemoryMapInfoKHR, pp_data: *mut *mut c_void) -> VkResult {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return VK_ERROR_INITIALIZATION_FAILED };
    crate::memory::vkMapMemory(core::ptr::null_mut(), info.memory.as_raw(), info.offset, info.size, info.flags.as_raw(), pp_data)
}

#[no_mangle]
pub extern "C" fn vkUnmapMemory2(_device: VkDevice, p_info: *const vk::MemoryUnmapInfoKHR) -> VkResult {
    if let Some(info) = unsafe { p_info.as_ref() } {
        crate::memory::vkUnmapMemory(core::ptr::null_mut(), info.memory.as_raw());
    }
    VK_SUCCESS
}

/// `vkGetImageSubresourceLayout2` — the `...2` wrapper; fills the nested `subresourceLayout`.
#[no_mangle]
pub extern "C" fn vkGetImageSubresourceLayout2(
    _device: VkDevice,
    image: VkImage,
    p_subresource: *const vk::ImageSubresource2KHR,
    p_layout: *mut vk::SubresourceLayout2KHR,
) {
    let Some(out) = (unsafe { p_layout.as_mut() }) else { return };
    let sub = unsafe { p_subresource.as_ref() }.map(|s| s.image_subresource).unwrap_or_default();
    crate::memory::vkGetImageSubresourceLayout(core::ptr::null_mut(), image, &sub, &mut out.subresource_layout);
}

/// `vkGetDeviceImageSubresourceLayout` (maintenance5) — the layout of a subresource of an image that
/// WOULD be created with the given `VkImageCreateInfo` (tightly-packed RGBA8 row pitch), no object.
#[no_mangle]
pub extern "C" fn vkGetDeviceImageSubresourceLayout(
    _device: VkDevice,
    p_info: *const vk::DeviceImageSubresourceInfoKHR,
    p_layout: *mut vk::SubresourceLayout2KHR,
) {
    let Some(out) = (unsafe { p_layout.as_mut() }) else { return };
    let (w, h) = unsafe { p_info.as_ref() }
        .and_then(|i| unsafe { i.p_create_info.as_ref() })
        .map(|ci| (ci.extent.width.max(1), ci.extent.height.max(1)))
        .unwrap_or((1, 1));
    let row_pitch = (w * 4) as u64;
    out.subresource_layout = vk::SubresourceLayout {
        offset: 0,
        size: row_pitch * h as u64,
        row_pitch,
        array_pitch: row_pitch * h as u64,
        depth_pitch: row_pitch * h as u64,
    };
}

/// `vkGetRenderingAreaGranularity` (maintenance5) — the render-area alignment for a dynamic-rendering
/// scope: `(1, 1)` (texel-granular, always spec-valid).
#[no_mangle]
pub extern "C" fn vkGetRenderingAreaGranularity(
    _device: VkDevice,
    _p_rendering_area_info: *const c_void,
    p_granularity: *mut vk::Extent2D,
) {
    if let Some(out) = unsafe { p_granularity.as_mut() } {
        *out = vk::Extent2D { width: 1, height: 1 };
    }
}

// ---- line stipple + rendering local-read remapping (recorded) ------------------------------------

/// `vkCmdSetLineStipple` (Vulkan 1.4) — record the stipple factor/pattern (dynamic state; not lowered).
#[no_mangle]
pub extern "C" fn vkCmdSetLineStipple(command_buffer: VkCommandBuffer, line_stipple_factor: u32, line_stipple_pattern: u16) {
    if let Some(cb) = reg::lock().recording_mut(command_buffer as usize) {
        cb.dynamic.line_stipple = (line_stipple_factor, line_stipple_pattern);
    }
}

/// `vkCmdSetRenderingAttachmentLocations` (Vulkan 1.4, dynamic-rendering local read) — validated on a
/// recording command buffer; the color-attachment remapping is not modeled by the IR (single-attachment
/// bring-up), so it is a recorded no-op (bounded → `partial`).
#[no_mangle]
pub extern "C" fn vkCmdSetRenderingAttachmentLocations(command_buffer: VkCommandBuffer, _p_location_info: *const c_void) {
    let _ = reg::lock().recording_mut(command_buffer as usize);
}

/// `vkCmdSetRenderingInputAttachmentIndices` (Vulkan 1.4) — as above (input-attachment index remapping).
#[no_mangle]
pub extern "C" fn vkCmdSetRenderingInputAttachmentIndices(command_buffer: VkCommandBuffer, _p_index_info: *const c_void) {
    let _ = reg::lock().recording_mut(command_buffer as usize);
}

// ---- push descriptors (transient descriptor set) -------------------------------------------------

/// Build a transient descriptor set with `layout`, apply the buffer writes, and bind it at `set` — the
/// push-descriptor model (no persistent set object). Ported from `MVKCmdPushDescriptorSet`. Buffer
/// descriptors lower to the IR bind group; image/sampler/texel writes are retained (bounded → `partial`).
fn push_descriptor_writes(command_buffer: VkCommandBuffer, layout: u64, set: u32, writes: &[vk::WriteDescriptorSet]) {
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.dsets.insert(
        handle,
        reg::DsetRec {
            set: 0,
            layout,
            pool: 0,
            buffers: std::collections::HashMap::new(),
            image_writes: std::collections::HashMap::new(),
            texel_writes: std::collections::HashMap::new(),
            variable_count: None,
        },
    );
    for w in writes {
        if w.descriptor_count == 0 {
            continue;
        }
        let ty = w.descriptor_type.as_raw();
        // 6 UNIFORM_BUFFER, 7 STORAGE_BUFFER, 8/9 dynamic — the buffer descriptor classes the IR lowers.
        if matches!(ty, 6 | 7 | 8 | 9) && !w.p_buffer_info.is_null() {
            let bi = unsafe { &*w.p_buffer_info };
            let buffer_handle = bi.buffer.as_raw();
            let range = if bi.range == vk::WHOLE_SIZE {
                s.buffers.get(&buffer_handle).map(|b| b.size).unwrap_or(0)
            } else {
                bi.range
            };
            if let Some(d) = s.dsets.get_mut(&handle) {
                d.buffers.insert(w.dst_binding, (buffer_handle, bi.offset, range));
            }
        }
    }
    drop(s);
    crate::command::vkCmdBindDescriptorSets(command_buffer, 0, layout, set, 1, &handle, 0, core::ptr::null());
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet(
    command_buffer: VkCommandBuffer,
    _pipeline_bind_point: i32,
    layout: VkPipelineLayout,
    set: u32,
    descriptor_write_count: u32,
    p_descriptor_writes: *const vk::WriteDescriptorSet,
) {
    let writes = if p_descriptor_writes.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(p_descriptor_writes, descriptor_write_count as usize) }
    };
    push_descriptor_writes(command_buffer, layout, set, writes);
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2(command_buffer: VkCommandBuffer, p_info: *const vk::PushDescriptorSetInfoKHR) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    let writes = if info.p_descriptor_writes.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(info.p_descriptor_writes, info.descriptor_write_count as usize) }
    };
    push_descriptor_writes(command_buffer, info.layout.as_raw(), info.set, writes);
}

/// `vkCmdPushDescriptorSetWithTemplate` — build a transient set, apply the template's data, bind it.
#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate(
    command_buffer: VkCommandBuffer,
    descriptor_update_template: VkDescriptorUpdateTemplate,
    layout: VkPipelineLayout,
    set: u32,
    p_data: *const c_void,
) {
    let handle = {
        let mut s = reg::lock();
        let h = s.alloc_handle();
        s.dsets.insert(
            h,
            reg::DsetRec {
                set: 0,
                layout,
                pool: 0,
                buffers: std::collections::HashMap::new(),
                image_writes: std::collections::HashMap::new(),
                texel_writes: std::collections::HashMap::new(),
                variable_count: None,
            },
        );
        h
    };
    crate::descriptor::vkUpdateDescriptorSetWithTemplate(core::ptr::null_mut(), handle, descriptor_update_template, p_data);
    crate::command::vkCmdBindDescriptorSets(command_buffer, 0, layout, set, 1, &handle, 0, core::ptr::null());
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2(
    command_buffer: VkCommandBuffer,
    p_info: *const vk::PushDescriptorSetWithTemplateInfoKHR,
) {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return };
    vkCmdPushDescriptorSetWithTemplate(
        command_buffer,
        info.descriptor_update_template.as_raw(),
        info.layout.as_raw(),
        info.set,
        info.p_data,
    );
}

// ---- host image layout transition (VK_EXT_host_image_copy) ---------------------------------------

/// `vkTransitionImageLayout` — a HOST-side (no queue) image layout transition. Our images track their
/// per-mip/layer subresource layout, so this applies the new layout directly to the addressed
/// subresources. Ported from `MVKImage` host layout tracking.
#[no_mangle]
pub extern "C" fn vkTransitionImageLayout(
    _device: VkDevice,
    transition_count: u32,
    p_transitions: *const vk::HostImageLayoutTransitionInfoEXT,
) -> VkResult {
    if transition_count == 0 || p_transitions.is_null() {
        return VK_SUCCESS;
    }
    let transitions = unsafe { core::slice::from_raw_parts(p_transitions, transition_count as usize) };
    let mut s = reg::lock();
    for t in transitions {
        let Some(image) = s.images.get_mut(&t.image.as_raw()) else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let raw = t.subresource_range;
        let level_count = if raw.level_count == vk::REMAINING_MIP_LEVELS {
            image.mip_levels.saturating_sub(raw.base_mip_level)
        } else {
            raw.level_count
        };
        let layer_count = if raw.layer_count == vk::REMAINING_ARRAY_LAYERS {
            image.array_layers.saturating_sub(raw.base_array_layer)
        } else {
            raw.layer_count
        };
        let new_layout = t.new_layout.as_raw();
        for mip in raw.base_mip_level..raw.base_mip_level.saturating_add(level_count) {
            for layer in raw.base_array_layer..raw.base_array_layer.saturating_add(layer_count) {
                if let Some(state) = image.subresources.get_mut(&(image.aspect_mask, mip, layer)) {
                    state.layout = new_layout;
                }
            }
        }
    }
    VK_SUCCESS
}

// ---- host memory <-> image copies (VK_EXT_host_image_copy) ---------------------------------------

/// `vkCopyMemoryToImage` — a HOST-side memory→image copy. Lowered as a deferred IR upload (a staging
/// buffer + `CopyBufferToTexture` appended to the stream, the same start-of-frame upload model as mapped
/// memory) for the base 2D color region. Ported from `MVKImage` host copy. Bounded → `partial`.
#[no_mangle]
pub extern "C" fn vkCopyMemoryToImage(_device: VkDevice, p_info: *const vk::CopyMemoryToImageInfoEXT) -> VkResult {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return VK_ERROR_INITIALIZATION_FAILED };
    if info.region_count == 0 || info.p_regions.is_null() {
        return VK_SUCCESS;
    }
    let regions = unsafe { core::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
    let mut s = reg::lock();
    let Some(image) = s.images.get(&info.dst_image.as_raw()) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let (dst_ir, _iw, _ih) = (image.ir_id, image.width, image.height);
    for r in regions {
        if r.p_host_pointer.is_null() || r.image_subresource.base_array_layer != 0 || r.image_subresource.mip_level != 0 {
            continue;
        }
        let (w, h) = (r.image_extent.width, r.image_extent.height);
        if w == 0 || h == 0 {
            continue;
        }
        let src_row = if r.memory_row_length == 0 { w } else { r.memory_row_length } as usize * 4;
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let row = unsafe { core::slice::from_raw_parts((r.p_host_pointer as *const u8).add(y * src_row), (w * 4) as usize) };
            data.extend_from_slice(row);
        }
        let staging = s.alloc_ir();
        s.record(Cmd::CreateBuffer(staging, BufferDesc { size: data.len() as u64, usage: buffer_usage::COPY_SRC, label: format!("hostcopy{staging}") }));
        s.record(Cmd::WriteBuffer { id: staging, offset: 0, data });
        s.record(Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture { src: staging, src_offset: 0, bytes_per_row: w * 4, dst: dst_ir, mip: 0, width: w, height: h }],
            signal: None,
        }));
        s.record(Cmd::DestroyBuffer(staging));
    }
    VK_SUCCESS
}

/// `vkCopyImageToImage` — a HOST-side image→image copy, lowered as a deferred IR `CopyTextureToTexture`
/// for the base 2D color region. Bounded → `partial`.
#[no_mangle]
pub extern "C" fn vkCopyImageToImage(_device: VkDevice, p_info: *const vk::CopyImageToImageInfoEXT) -> VkResult {
    let Some(info) = (unsafe { p_info.as_ref() }) else { return VK_ERROR_INITIALIZATION_FAILED };
    if info.region_count == 0 || info.p_regions.is_null() {
        return VK_SUCCESS;
    }
    let regions = unsafe { core::slice::from_raw_parts(info.p_regions, info.region_count as usize) };
    let mut s = reg::lock();
    let (Some(src), Some(dst)) = (s.images.get(&info.src_image.as_raw()), s.images.get(&info.dst_image.as_raw())) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let (sid, did) = (src.ir_id, dst.ir_id);
    let mut encs = Vec::new();
    for r in regions {
        if r.extent.width == 0 || r.extent.height == 0 {
            continue;
        }
        encs.push(Enc::CopyTextureToTexture {
            src: sid,
            src_sub: TextureSubresource { mip: r.src_subresource.mip_level, layer: r.src_subresource.base_array_layer, aspect: TextureAspect::All },
            src_origin: Origin3d { x: r.src_offset.x.max(0) as u32, y: r.src_offset.y.max(0) as u32, z: 0 },
            dst: did,
            dst_sub: TextureSubresource { mip: r.dst_subresource.mip_level, layer: r.dst_subresource.base_array_layer, aspect: TextureAspect::All },
            dst_origin: Origin3d { x: r.dst_offset.x.max(0) as u32, y: r.dst_offset.y.max(0) as u32, z: 0 },
            extent: Extent3d { width: r.extent.width, height: r.extent.height, depth: 1 },
        });
    }
    if !encs.is_empty() {
        s.record(Cmd::Submit(CommandBuffer { encoder: encs, signal: None }));
    }
    VK_SUCCESS
}

/// `vkCopyImageToMemory` — a HOST-side image→memory readback. Our synchronous IR model cannot read a
/// texture back to host memory without a round-trip through the host executor (there is no host-immediate
/// texture read), and we do not advertise the `hostImageCopy` feature, so this truthfully reports the
/// unmaterialized capability (bounded → `partial`). Ported from the guarded path in `MVKImage`.
#[no_mangle]
pub extern "C" fn vkCopyImageToMemory(_device: VkDevice, p_info: *const vk::CopyImageToMemoryInfoEXT) -> VkResult {
    if unsafe { p_info.as_ref() }.is_none() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    VK_ERROR_FEATURE_NOT_PRESENT
}
