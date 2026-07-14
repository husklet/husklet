//! The IR-wired GRAPHICS path: the hand-written `vk*` bodies that marshal the Vulkan C ABI and call the
//! `hl_vulkan` lowering services (`create`/`record`/`present`) — images/views/samplers, render passes +
//! framebuffers, graphics pipelines, the `vkCmd*` render-pass recording, and the WSI present chain.
//!
//! These are the SAME lowering `tests/lowering.rs` + `tests/e2e.rs` exercise in-process, reached here
//! across the C ABI. Every body is panic-free across the seam (raw pointers null-checked, a lowering
//! [`hl_gpu::GpuError`] mapped to the accurate `VkResult` via [`hl_vulkan::result`], never a false
//! `VK_SUCCESS`). Objects the neutral `hl_vulkan` object model does not itself carry — image views,
//! render passes, framebuffers, semaphores — are thin bring-up bookkeeping in [`crate::state`], resolved
//! back to the modeled resources (a view → its image; a framebuffer+render-pass → the color image +
//! clear behaviour) when the recording lands.

use core::ffi::{c_char, c_void};

use hl_gpu::protocol::model::descriptor::{VertexAttr, VertexLayout};
use hl_gpu::protocol::model::enums::{texture_usage, TextureFormat};
use hl_gpu::CommandSink;
use hl_vulkan::model::memory::{tex_format_from_vk, ImageRec};
use hl_vulkan::model::queue::PRESENT_TEXTURE_ID;
use hl_vulkan::result::vk_result_from_gpu_error;
use hl_vulkan::service::{create, present, record};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::{with, RenderPassRec};
use crate::types::*;

// ---- shared marshalling helpers ------------------------------------------------------------------

/// Run `f` with the logical device + the command sink (disjoint `State` fields). `None` if no device
/// has been created yet — the caller maps that to `VK_ERROR_INITIALIZATION_FAILED`.
fn dev_sink<R>(f: impl FnOnce(&mut Device, &mut dyn CommandSink) -> R) -> Option<R> {
    with(|s| {
        let sink = &mut s.sink;
        let dev = s.device.as_mut()?;
        Some(f(dev, sink))
    })
}

/// Run `f` with just the logical device (for pure-bookkeeping / recording bodies that emit no `Cmd`).
fn dev<R>(f: impl FnOnce(&mut Device) -> R) -> Option<R> {
    with(|s| s.device.as_mut().map(f))
}

/// Borrow a nul-terminated C string as `&str` (`"main"` fallback on NULL / bad UTF-8).
unsafe fn entry_str<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "main";
    }
    core::ffi::CStr::from_ptr(p).to_str().unwrap_or("main")
}

/// Unwrap a dispatchable `VkCommandBuffer` to its `hl_vulkan` `u64` command-buffer handle.
unsafe fn cmdbuf_handle(p: *mut c_void) -> Option<VkCbHandle> {
    Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
}

// ==================================================================================================
// images + image views + samplers
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateImage(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_image: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkImageCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_image.is_null() {
        unsafe { *p_image = 0 };
    }
    dev_sink(|dev, sink| {
        match create::create_image(
            dev,
            sink,
            ci.extent.width,
            ci.extent.height.max(1),
            ci.format as u32,
            ci.usage,
        ) {
            Ok(h) => {
                if !p_image.is_null() {
                    unsafe { *p_image = h };
                }
                VK_SUCCESS
            }
            Err(e) => vk_result_from_gpu_error(&e),
        }
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkDestroyImage(_device: *mut c_void, image: u64, _p_allocator: *const c_void) {
    dev(|dev| {
        dev.images.remove(&image);
    });
}

#[no_mangle]
pub extern "C" fn vkGetImageMemoryRequirements(
    _device: *mut c_void,
    image: u64,
    p_memory_requirements: *mut c_void,
) {
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements).as_mut() }) else {
        return;
    };
    // A render-target image is host-owned; report a plausible footprint (width*height*4) so a probing
    // app's allocation math is sane, even though hl binds no VkDeviceMemory to it.
    let size = dev(|dev| dev.images.get(&image).map(|i| i.width as u64 * i.height as u64 * 4).unwrap_or(0))
        .unwrap_or(0);
    out.size = size;
    out.alignment = 256;
    out.memory_type_bits = 1;
}

#[no_mangle]
pub extern "C" fn vkBindImageMemory(
    _device: *mut c_void,
    _image: u64,
    _memory: u64,
    _memory_offset: u64,
) -> VkResult {
    // Images are host-owned render targets in this model (no explicit VkDeviceMemory backing); the bind
    // is a no-op that succeeds so a conventional create→bind flow proceeds.
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkCreateImageView(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_view: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkImageViewCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_view.is_null() {
        unsafe { *p_view = 0 };
    }
    // A view is a thin alias of its image (the hl model renders into images directly); record the
    // view→image mapping so vkCmdBeginRenderPass can resolve a framebuffer attachment back to its image.
    let handle = with(|s| {
        let h = s.device.as_mut()?.alloc_handle();
        s.image_views.insert(h, ci.image);
        Some(h)
    });
    match handle {
        Some(h) => {
            if !p_view.is_null() {
                unsafe { *p_view = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyImageView(_device: *mut c_void, image_view: u64, _p_allocator: *const c_void) {
    with(|s| {
        s.image_views.remove(&image_view);
    });
}

#[no_mangle]
pub extern "C" fn vkCreateSampler(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_sampler: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkSamplerCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let h = dev_sink(|dev, sink| {
        create::create_sampler(
            dev,
            sink,
            ci.min_filter as u32,
            ci.mag_filter as u32,
            ci.mipmap_mode as u32,
            [ci.address_mode_u as u32, ci.address_mode_v as u32, ci.address_mode_w as u32],
        )
    });
    match h {
        Some(handle) => {
            if !p_sampler.is_null() {
                unsafe { *p_sampler = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroySampler(_device: *mut c_void, sampler: u64, _p_allocator: *const c_void) {
    dev(|dev| {
        dev.samplers.remove(&sampler);
    });
}

// ==================================================================================================
// render pass + framebuffer (bring-up bookkeeping resolved at vkCmdBeginRenderPass)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateRenderPass(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_render_pass: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkRenderPassCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // Record the first color attachment's clear behaviour + format (the bring-up single-target subset).
    let (clears, fmt) = if ci.p_attachments.is_null() || ci.attachment_count == 0 {
        (false, 0u32)
    } else {
        let a0 = unsafe { &*ci.p_attachments };
        (a0.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR, a0.format as u32)
    };
    let handle = with(|s| {
        let h = s.device.as_mut()?.alloc_handle();
        s.render_passes
            .insert(h, RenderPassRec { first_attachment_clears: clears, color_format_vk: fmt });
        Some(h)
    });
    match handle {
        Some(h) => {
            if !p_render_pass.is_null() {
                unsafe { *p_render_pass = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyRenderPass(_device: *mut c_void, render_pass: u64, _p_allocator: *const c_void) {
    with(|s| {
        s.render_passes.remove(&render_pass);
    });
}

#[no_mangle]
pub extern "C" fn vkCreateFramebuffer(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_framebuffer: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkFramebufferCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let views: Vec<u64> = if ci.p_attachments.is_null() || ci.attachment_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) }.to_vec()
    };
    let handle = with(|s| {
        let h = s.device.as_mut()?.alloc_handle();
        s.framebuffers.insert(h, views);
        Some(h)
    });
    match handle {
        Some(h) => {
            if !p_framebuffer.is_null() {
                unsafe { *p_framebuffer = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyFramebuffer(_device: *mut c_void, framebuffer: u64, _p_allocator: *const c_void) {
    with(|s| {
        s.framebuffers.remove(&framebuffer);
    });
}

// ==================================================================================================
// graphics pipeline
// ==================================================================================================

/// Translate a `VkPipelineVertexInputStateCreateInfo` into the neutral per-binding vertex layouts (the
/// host rasterizer fetches slot-0 positions/colors from these).
fn parse_vertex_layouts(vi: *const VkPipelineVertexInputStateCreateInfo) -> Vec<VertexLayout> {
    let Some(vi) = (unsafe { vi.as_ref() }) else {
        return Vec::new();
    };
    let bindings: &[VkVertexInputBindingDescription] =
        if vi.p_vertex_binding_descriptions.is_null() || vi.vertex_binding_description_count == 0 {
            &[]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    vi.p_vertex_binding_descriptions,
                    vi.vertex_binding_description_count as usize,
                )
            }
        };
    let attrs: &[VkVertexInputAttributeDescription] =
        if vi.p_vertex_attribute_descriptions.is_null() || vi.vertex_attribute_description_count == 0 {
            &[]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    vi.p_vertex_attribute_descriptions,
                    vi.vertex_attribute_description_count as usize,
                )
            }
        };
    bindings
        .iter()
        .map(|b| VertexLayout {
            stride: b.stride,
            step_mode: b.input_rate as u32,
            attrs: attrs
                .iter()
                .filter(|a| a.binding == b.binding)
                .map(|a| VertexAttr { location: a.location, format: a.format as u32, offset: a.offset })
                .collect(),
        })
        .collect()
}

#[no_mangle]
pub extern "C" fn vkCreateGraphicsPipelines(
    _device: *mut c_void,
    _pipeline_cache: u64,
    create_info_count: u32,
    p_create_infos: *const c_void,
    _p_allocator: *const c_void,
    p_pipelines: *mut u64,
) -> VkResult {
    if p_create_infos.is_null() || p_pipelines.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(
            p_create_infos as *const VkGraphicsPipelineCreateInfo,
            create_info_count as usize,
        )
    };
    let out = unsafe { std::slice::from_raw_parts_mut(p_pipelines, create_info_count as usize) };
    let mut result = VK_SUCCESS;
    for (i, ci) in infos.iter().enumerate() {
        out[i] = 0;
        // Resolve the vertex (+ optional fragment) stage module + entry from pStages.
        let stages: &[VkPipelineShaderStageCreateInfo] = if ci.p_stages.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(ci.p_stages, ci.stage_count as usize) }
        };
        let mut vertex: Option<(u64, String)> = None;
        let mut fragment: Option<(u64, String)> = None;
        for st in stages {
            let entry = unsafe { entry_str(st.p_name) }.to_string();
            if st.stage & VK_SHADER_STAGE_VERTEX_BIT != 0 {
                vertex = Some((st.module, entry));
            } else if st.stage & VK_SHADER_STAGE_FRAGMENT_BIT != 0 {
                fragment = Some((st.module, entry));
            }
        }
        let Some((vmod, ventry)) = vertex else {
            result = VK_ERROR_UNKNOWN;
            continue;
        };
        let layouts = parse_vertex_layouts(ci.p_vertex_input_state);
        // The single color target's format comes from the bound render pass's first attachment.
        let color_fmt = with(|s| {
            s.render_passes.get(&ci.render_pass).map(|r| tex_format_from_vk(r.color_format_vk))
        })
        .unwrap_or(TextureFormat::Rgba8Unorm);

        let r = dev_sink(|dev, sink| {
            let frag = fragment.as_ref().map(|(m, e)| (*m, e.as_str()));
            create::create_graphics_pipeline(dev, sink, (vmod, ventry.as_str()), frag, layouts, color_fmt)
        })
        .unwrap_or(Err(hl_gpu::GpuError::Invalid("vkCreateGraphicsPipelines: no device")));
        match r {
            Ok(h) => out[i] = h,
            Err(e) => result = vk_result_from_gpu_error(&e),
        }
    }
    result
}

// ==================================================================================================
// render-pass command recording
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdBeginRenderPass(
    command_buffer: *mut c_void,
    p_render_pass_begin: *const c_void,
    _contents: i32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    let Some(bi) = (unsafe { (p_render_pass_begin as *const VkRenderPassBeginInfo).as_ref() }) else {
        return;
    };
    // The clear color is the first pClearValues entry (color aspect); default opaque black.
    let clear = if bi.p_clear_values.is_null() || bi.clear_value_count == 0 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        unsafe { (*bi.p_clear_values).float32 }
    };
    with(|s| {
        // Resolve framebuffer → first attachment view → image handle; render pass → clear behaviour.
        let image = s
            .framebuffers
            .get(&bi.framebuffer)
            .and_then(|v| v.first().copied())
            .and_then(|view| s.image_views.get(&view).copied());
        let clears = s.render_passes.get(&bi.render_pass).map(|r| r.first_attachment_clears).unwrap_or(true);
        let Some(image) = image else { return };
        if let Some(dev) = s.device.as_mut() {
            let _ = record::cmd_begin_render_pass(dev, cb, image, clear, clears);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdEndRenderPass(command_buffer: *mut c_void) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|dev| {
        let _ = record::cmd_end_render_pass(dev, cb);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdBindVertexBuffers(
    command_buffer: *mut c_void,
    first_binding: u32,
    binding_count: u32,
    p_buffers: *const u64,
    p_offsets: *const u64,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    if p_buffers.is_null() {
        return;
    }
    let buffers = unsafe { std::slice::from_raw_parts(p_buffers, binding_count as usize) };
    let offsets = if p_offsets.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_offsets, binding_count as usize) }.to_vec()
    };
    dev(|dev| {
        for (i, &buf) in buffers.iter().enumerate() {
            let slot = first_binding + i as u32;
            let offset = offsets.get(i).copied().unwrap_or(0);
            let _ = record::cmd_bind_vertex_buffer(dev, cb, slot, buf, offset);
        }
    });
}

#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    index_type: i32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|dev| {
        let _ = record::cmd_bind_index_buffer(dev, cb, buffer, offset, index_type as u32);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdDraw(
    command_buffer: *mut c_void,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|dev| {
        let _ = record::cmd_draw(dev, cb, vertex_count, instance_count, first_vertex, first_instance);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdDrawIndexed(
    command_buffer: *mut c_void,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|dev| {
        let _ = record::cmd_draw_indexed(
            dev,
            cb,
            index_count,
            instance_count,
            first_index,
            vertex_offset,
            first_instance,
        );
    });
}

// ==================================================================================================
// WSI: swapchain + present
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateSwapchainKHR(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_swapchain: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkSwapchainCreateInfoKHR).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_swapchain.is_null() {
        unsafe { *p_swapchain = 0 };
    }
    // Bring-up: materialize the presentation surface from the swapchain extent/format (hlp surface 0),
    // then register the swapchain's presentable images against it.
    let r = dev_sink(|dev, sink| {
        let surface = create_surface_for_swapchain(dev, sink, ci)?;
        present::create_swapchain(dev, surface, ci.min_image_count)
    });
    match r {
        Some(Ok(h)) => {
            if !p_swapchain.is_null() {
                unsafe { *p_swapchain = h };
            }
            VK_SUCCESS
        }
        Some(Err(e)) => vk_result_from_gpu_error(&e),
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// Create the GPU surface a swapchain presents through (extent/format from the swapchain create info).
fn create_surface_for_swapchain(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    ci: &VkSwapchainCreateInfoKHR,
) -> hl_gpu::Result<u64> {
    present::create_surface(
        dev,
        sink,
        ci.image_extent.width,
        ci.image_extent.height,
        ci.image_format as u32,
        0,
    )
}

#[no_mangle]
pub extern "C" fn vkDestroySwapchainKHR(_device: *mut c_void, swapchain: u64, _p_allocator: *const c_void) {
    dev(|dev| {
        dev.swapchains.remove(&swapchain);
    });
}

#[no_mangle]
pub extern "C" fn vkGetSwapchainImagesKHR(
    _device: *mut c_void,
    swapchain: u64,
    p_swapchain_image_count: *mut u32,
    p_swapchain_images: *mut u64,
) -> VkResult {
    if p_swapchain_image_count.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    dev(|dev| {
        let Some((w, h, fmt, count)) = dev
            .swapchains
            .get(&swapchain)
            .map(|sc| (sc.width, sc.height, sc.format, sc.images.len() as u32))
        else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        if p_swapchain_images.is_null() {
            unsafe { *p_swapchain_image_count = count };
            return VK_SUCCESS;
        }
        let cap = unsafe { *p_swapchain_image_count };
        let n = cap.min(count);
        let out = unsafe { std::slice::from_raw_parts_mut(p_swapchain_images, n as usize) };
        for slot in out.iter_mut() {
            // Each presentable image resolves to the reserved present texture id; mint a VkImage handle
            // so views/framebuffers/render-passes over it lower correctly.
            let handle = dev.alloc_handle();
            dev.images.insert(
                handle,
                ImageRec {
                    ir_id: PRESENT_TEXTURE_ID,
                    width: w,
                    height: h,
                    format: fmt,
                    usage: texture_usage::RENDER_TARGET,
                    is_render_target: true,
                },
            );
            *slot = handle;
        }
        unsafe { *p_swapchain_image_count = n };
        if n < count {
            VK_INCOMPLETE
        } else {
            VK_SUCCESS
        }
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkAcquireNextImageKHR(
    _device: *mut c_void,
    swapchain: u64,
    _timeout: u64,
    _semaphore: u64,
    _fence: u64,
    p_image_index: *mut u32,
) -> VkResult {
    dev(|dev| match present::acquire_next_image(dev, swapchain) {
        Ok(idx) => {
            if !p_image_index.is_null() {
                unsafe { *p_image_index = idx };
            }
            VK_SUCCESS
        }
        Err(e) => vk_result_from_gpu_error(&e),
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkQueuePresentKHR(_queue: *mut c_void, p_present_info: *const c_void) -> VkResult {
    let Some(pi) = (unsafe { (p_present_info as *const VkPresentInfoKHR).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if pi.p_swapchains.is_null() || pi.p_image_indices.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let swapchains = unsafe { std::slice::from_raw_parts(pi.p_swapchains, pi.swapchain_count as usize) };
    let indices = unsafe { std::slice::from_raw_parts(pi.p_image_indices, pi.swapchain_count as usize) };
    dev_sink(|dev, sink| {
        let mut res = VK_SUCCESS;
        for (sc, &idx) in swapchains.iter().zip(indices) {
            if let Err(e) = present::queue_present(dev, sink, *sc, idx) {
                res = vk_result_from_gpu_error(&e);
            }
        }
        res
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

// ==================================================================================================
// semaphores (present/acquire sync — bookkeeping only for the synchronous executor)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateSemaphore(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_semaphore: *mut u64,
) -> VkResult {
    let handle = with(|s| {
        let h = s.device.as_mut()?.alloc_handle();
        s.semaphores.insert(h, ());
        Some(h)
    });
    match handle {
        Some(h) => {
            if !p_semaphore.is_null() {
                unsafe { *p_semaphore = h };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroySemaphore(_device: *mut c_void, semaphore: u64, _p_allocator: *const c_void) {
    with(|s| {
        s.semaphores.remove(&semaphore);
    });
}
