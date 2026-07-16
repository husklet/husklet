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

use hl_gpu::protocol::model::descriptor::{BlendState, DepthState, VertexAttr, VertexLayout};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::CommandSink;
use hl_vulkan::adapter::wayland_app::WaylandAppPresenter;
use hl_vulkan::model::memory::tex_format_from_vk;
use hl_vulkan::result::vk_result_from_gpu_error;
use hl_vulkan::service::record::{RenderingColorAttachment, RenderingDepthAttachment};
use hl_vulkan::service::{create, present, record};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};

use crate::state::{with, RenderPassDepth, RenderPassRec, WaylandWindow};
use crate::types::*;
use hl_log::tag;

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
            // `VkImageCreateInfo::samples` is a `VkSampleCountFlagBits` whose bit VALUE is the sample count.
            // `create::create_image` collapses 0 to single-sample, so an absent field stays byte-identical.
            ci.samples as u32,
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

/// `vkGetImageSubresourceLayout` — report the linear byte layout (offset/size/rowPitch) of `image`'s
/// subresource. Modeled images are single-mip single-layer RGBA8 2D targets (rowPitch = width*4). Leaves
/// the output zeroed on an unknown image (the caller must have queried a valid, linear-tiled image).
#[no_mangle]
pub extern "C" fn vkGetImageSubresourceLayout(
    _device: *mut c_void,
    image: u64,
    _p_subresource: *const c_void,
    p_layout: *mut c_void,
) {
    let Some(out) = (unsafe { (p_layout as *mut VkSubresourceLayout).as_mut() }) else {
        return;
    };
    *out = VkSubresourceLayout::default();
    if let Some(Ok(l)) = dev(|d| create::image_subresource_layout(d, image)) {
        out.offset = l.offset;
        out.size = l.size;
        out.row_pitch = l.row_pitch;
        out.array_pitch = l.array_pitch;
        out.depth_pitch = l.depth_pitch;
    }
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

// ---- bind-memory-2 / memory-requirements-2 for images (core 1.1 / KHR) — delegate to the v1 bodies

/// `vkBindImageMemory2` — bind each `VkBindImageMemoryInfo` via the v1 [`vkBindImageMemory`] body (a
/// host-owned render-target image binds as a no-op success). Returns the first error (else `VK_SUCCESS`).
#[no_mangle]
pub extern "C" fn vkBindImageMemory2(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    if p_bind_infos.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(p_bind_infos as *const VkBindImageMemoryInfo, bind_info_count as usize)
    };
    let mut result = VK_SUCCESS;
    for bi in infos {
        let r = vkBindImageMemory(device, bi.image, bi.memory, bi.memory_offset);
        if r != VK_SUCCESS {
            result = r;
        }
    }
    result
}

/// `vkBindImageMemory2KHR` — the `VK_KHR_bind_memory2` alias of [`vkBindImageMemory2`].
#[no_mangle]
pub extern "C" fn vkBindImageMemory2KHR(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    vkBindImageMemory2(device, bind_info_count, p_bind_infos)
}

/// `vkGetImageMemoryRequirements2` — read `VkImageMemoryRequirementsInfo2` and fill the base
/// `VkMemoryRequirements` via the v1 [`vkGetImageMemoryRequirements`] body (chain preserved).
#[no_mangle]
pub extern "C" fn vkGetImageMemoryRequirements2(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    let Some(info) = (unsafe { (p_info as *const VkImageMemoryRequirementsInfo2).as_ref() }) else {
        return;
    };
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements2).as_mut() }) else {
        return;
    };
    vkGetImageMemoryRequirements(
        device,
        info.image,
        &mut out.memory_requirements as *mut _ as *mut c_void,
    );
}

/// `vkGetImageMemoryRequirements2KHR` — the `VK_KHR_get_memory_requirements2` alias.
#[no_mangle]
pub extern "C" fn vkGetImageMemoryRequirements2KHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    vkGetImageMemoryRequirements2(device, p_info, p_memory_requirements)
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

/// Whether a raw `VkFormat` is a depth/stencil format — the contiguous `VK_FORMAT_D16_UNORM`(124) …
/// `VK_FORMAT_D32_SFLOAT_S8_UINT`(130) block (127 = `S8_UINT` is stencil-only, still a depth/stencil
/// attachment). Used to pick the depth attachment out of a classic render pass's attachment table.
fn is_depth_format(f: u32) -> bool {
    (124..=130).contains(&f)
}

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
    // Record the first color attachment's clear behaviour + format (the bring-up single-target subset), and
    // scan the attachment table for a depth/stencil attachment so the classic pass threads a real depth buffer.
    let (clears, fmt, depth) = if ci.p_attachments.is_null() || ci.attachment_count == 0 {
        (false, 0u32, None)
    } else {
        let atts = unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) };
        let a0 = &atts[0];
        let depth = atts.iter().enumerate().find(|(_, a)| is_depth_format(a.format as u32)).map(|(i, a)| {
            RenderPassDepth {
                index: i as u32,
                format_vk: a.format as u32,
                clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            }
        });
        (a0.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR, a0.format as u32, depth)
    };
    let handle = with(|s| {
        let h = s.device.as_mut()?.alloc_handle();
        s.render_passes
            .insert(h, RenderPassRec { first_attachment_clears: clears, color_format_vk: fmt, depth });
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

/// Walk a pNext chain for `VkPipelineRenderingCreateInfo` and read its `pColorAttachmentFormats` into the
/// neutral color-target formats (a dynamic-rendering pipeline's color targets). Empty when absent / no
/// color formats (a valid depth-only or no-color pipeline).
fn parse_pipeline_rendering_color_formats(p_next: *const c_void) -> Vec<TextureFormat> {
    let mut node = p_next as *const VkBaseInStructure;
    while let Some(n) = unsafe { node.as_ref() } {
        if n.s_type == VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO {
            let pr = unsafe { &*(node as *const VkPipelineRenderingCreateInfo) };
            if pr.p_color_attachment_formats.is_null() || pr.color_attachment_count == 0 {
                return Vec::new();
            }
            let fmts = unsafe {
                std::slice::from_raw_parts(pr.p_color_attachment_formats, pr.color_attachment_count as usize)
            };
            return fmts.iter().map(|&f| tex_format_from_vk(f as u32)).collect();
        }
        node = n.p_next;
    }
    Vec::new()
}

/// Walk a pNext chain for `VkPipelineRenderingCreateInfo` and read its `depthAttachmentFormat` — the depth
/// format a dynamic-rendering (null `renderPass`) pipeline targets. `None` when the struct is absent or the
/// format is `VK_FORMAT_UNDEFINED` (0), i.e. a color-only pipeline.
fn parse_pipeline_rendering_depth_format(p_next: *const c_void) -> Option<TextureFormat> {
    let mut node = p_next as *const VkBaseInStructure;
    while let Some(n) = unsafe { node.as_ref() } {
        if n.s_type == VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO {
            let pr = unsafe { &*(node as *const VkPipelineRenderingCreateInfo) };
            // VK_FORMAT_UNDEFINED (0) => no depth attachment.
            return (pr.depth_attachment_format != 0)
                .then(|| tex_format_from_vk(pr.depth_attachment_format as u32));
        }
        node = n.p_next;
    }
    None
}

/// Translate a `VkPipelineDepthStencilStateCreateInfo` into the neutral [`DepthState`] when the depth test
/// is enabled. `depth_format` is the pass's depth attachment format (from the dynamic-rendering pNext, or
/// `Depth32Float` as the bring-up default when unresolved). Returns `None` for a null state pointer or a
/// disabled depth test — exactly the pipelines that must NOT carry a depth attachment.
fn parse_depth_state(
    p_depth_stencil_state: *const c_void,
    depth_format: Option<TextureFormat>,
) -> Option<DepthState> {
    let ds = unsafe { (p_depth_stencil_state as *const VkPipelineDepthStencilStateCreateInfo).as_ref() }?;
    if ds.depth_test_enable == 0 {
        return None;
    }
    // VkCompareOp shares the neutral `compare::*` numeric ordering (NEVER=0 … ALWAYS=7). The bring-up
    // depth path models depth test/write only (no stencil), so use the depth-only DepthState shape.
    Some(DepthState::depth_only(
        depth_format.unwrap_or(TextureFormat::Depth32Float),
        ds.depth_write_enable != 0,
        ds.depth_compare_op as u32,
    ))
}

/// Translate a `VkBlendFactor` onto the neutral `hl_gpu` blend-factor wire numbering the GL driver emits
/// (0=ZERO 1=ONE 2=SRC_COLOR 3=1-SRC_COLOR 4=SRC_ALPHA 5=1-SRC_ALPHA 6=DST_COLOR 7=1-DST_COLOR 8=DST_ALPHA
/// 9=1-DST_ALPHA 10=SRC_ALPHA_SATURATE) that `hl_wip-gpu-wgpu`'s `blend_factor` decodes. VkBlendFactor
/// interleaves color/alpha differently (SRC_ALPHA=6, DST_COLOR=4) so the mapping is NOT identity. An
/// unmodeled factor defaults to ONE, matching the executor's own fallback rather than dropping the blend.
fn vk_blend_factor_wire(f: i32) -> u32 {
    match f {
        0 => 0,   // VK_BLEND_FACTOR_ZERO
        1 => 1,   // VK_BLEND_FACTOR_ONE
        2 => 2,   // VK_BLEND_FACTOR_SRC_COLOR
        3 => 3,   // VK_BLEND_FACTOR_ONE_MINUS_SRC_COLOR
        4 => 6,   // VK_BLEND_FACTOR_DST_COLOR
        5 => 7,   // VK_BLEND_FACTOR_ONE_MINUS_DST_COLOR
        6 => 4,   // VK_BLEND_FACTOR_SRC_ALPHA
        7 => 5,   // VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA
        8 => 8,   // VK_BLEND_FACTOR_DST_ALPHA
        9 => 9,   // VK_BLEND_FACTOR_ONE_MINUS_DST_ALPHA
        14 => 10, // VK_BLEND_FACTOR_SRC_ALPHA_SATURATE
        _ => 1,
    }
}

/// Translate a `VkBlendOp` onto the neutral blend-op wire numbering (0=ADD 1=SUBTRACT 2=REVERSE_SUBTRACT
/// 3=MIN 4=MAX). `VkBlendOp` (ADD=0 … MAX=4) already matches this ordering 1:1; an unmodeled op defaults
/// to ADD.
fn vk_blend_op_wire(o: i32) -> u32 {
    match o {
        1 => 1, // VK_BLEND_OP_SUBTRACT
        2 => 2, // VK_BLEND_OP_REVERSE_SUBTRACT
        3 => 3, // VK_BLEND_OP_MIN
        4 => 4, // VK_BLEND_OP_MAX
        _ => 0, // VK_BLEND_OP_ADD (and unmodeled)
    }
}

/// Translate a `VkPipelineColorBlendStateCreateInfo`'s FIRST attachment into the neutral [`BlendState`]
/// when that attachment's `blendEnable` is set. The software rasterizer models one blend for all color
/// targets, so only attachment 0 is read. Returns `None` for a null state pointer, an empty attachment
/// list, or `blendEnable = VK_FALSE` — exactly the pipelines that must OVERWRITE (opaque replace) rather
/// than composite. Without this the color-blend state was dropped (`blend: None` hardcoded) and a
/// translucent draw overwrote the destination instead of alpha-compositing over it.
fn parse_color_blend_state(p_color_blend_state: *const c_void) -> Option<BlendState> {
    let cb = unsafe { (p_color_blend_state as *const VkPipelineColorBlendStateCreateInfo).as_ref() }?;
    if cb.attachment_count == 0 || cb.p_attachments.is_null() {
        return None;
    }
    let att = unsafe { &*cb.p_attachments };
    if att.blend_enable == 0 {
        return None;
    }
    Some(BlendState {
        src_color: vk_blend_factor_wire(att.src_color_blend_factor),
        dst_color: vk_blend_factor_wire(att.dst_color_blend_factor),
        op_color: vk_blend_op_wire(att.color_blend_op),
        src_alpha: vk_blend_factor_wire(att.src_alpha_blend_factor),
        dst_alpha: vk_blend_factor_wire(att.dst_alpha_blend_factor),
        op_alpha: vk_blend_op_wire(att.alpha_blend_op),
    })
}

/// Read a `VkPipelineMultisampleStateCreateInfo`'s `rasterizationSamples` as the pipeline's multisample
/// count. `rasterizationSamples` is a `VkSampleCountFlagBits` whose bit VALUE is the count itself
/// (`_1_BIT`=1, `_2_BIT`=2, `_4_BIT`=4, `_8_BIT`=8, …), so the value is returned verbatim. A null pointer
/// (a pipeline with no multisample state — spec-legal for a rasterization-discard pipeline) or a `0`/`_1_BIT`
/// count folds to `1` (single-sample), keeping an existing non-MSAA pipeline byte-identical. Without this
/// the sample count was dropped (`sample_count: 1` hardcoded) and an MSAA pipeline rasterized single-sampled.
fn parse_multisample_samples(p_multisample_state: *const c_void) -> u32 {
    let Some(ms) = (unsafe { (p_multisample_state as *const VkPipelineMultisampleStateCreateInfo).as_ref() })
    else {
        return 1;
    };
    (ms.rasterization_samples as u32).max(1)
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
        // The color-target formats: from the bound VkRenderPass's attachment in the classic path, or —
        // for a VK_KHR_dynamic_rendering pipeline (null renderPass) — from the
        // VkPipelineRenderingCreateInfo::pColorAttachmentFormats carried in the pNext chain.
        let color_formats: Vec<TextureFormat> = if ci.render_pass == 0 {
            parse_pipeline_rendering_color_formats(ci.p_next)
        } else {
            let fmt = with(|s| {
                s.render_passes.get(&ci.render_pass).map(|r| tex_format_from_vk(r.color_format_vk))
            });
            vec![fmt.unwrap_or(TextureFormat::Rgba8Unorm)]
        };

        // Depth-test state: the depth attachment format comes from the dynamic-rendering pNext
        // (VkPipelineRenderingCreateInfo::depthAttachmentFormat) for a null-renderPass pipeline, or — for a
        // classic pipeline bound to a VkRenderPass — from that pass's declared depth attachment. Falls back to
        // Depth32Float when a depth-tested pipeline resolves no explicit format. A null pDepthStencilState /
        // disabled test => no depth.
        let depth_format = if ci.render_pass == 0 {
            parse_pipeline_rendering_depth_format(ci.p_next)
        } else {
            with(|s| {
                s.render_passes
                    .get(&ci.render_pass)
                    .and_then(|r| r.depth)
                    .map(|d| tex_format_from_vk(d.format_vk))
            })
        };
        let depth = parse_depth_state(ci.p_depth_stencil_state, depth_format);

        // Color-blend state: the first attachment's blendEnable + factors/ops, mapped onto the neutral
        // blend wire numbering. A null pColorBlendState / blendEnable = VK_FALSE => None (opaque overwrite),
        // preserving the pre-blend behavior.
        let blend = parse_color_blend_state(ci.p_color_blend_state);

        // Multisample state: rasterizationSamples → the pipeline's MSAA count. Null / _1_BIT => single-sample.
        let sample_count = parse_multisample_samples(ci.p_multisample_state);

        let r = dev_sink(|dev, sink| {
            let frag = fragment.as_ref().map(|(m, e)| (*m, e.as_str()));
            create::create_graphics_pipeline(dev, sink, (vmod, ventry.as_str()), frag, layouts, color_formats, depth, blend, sample_count)
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
    // The per-attachment clear values are indexed by attachment slot (color at 0, depth at its own slot).
    let clear_values: &[VkClearValue] = if bi.p_clear_values.is_null() || bi.clear_value_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bi.p_clear_values, bi.clear_value_count as usize) }
    };
    with(|s| {
        // Resolve framebuffer → first attachment view → image handle; render pass → clear behaviour.
        let views = s.framebuffers.get(&bi.framebuffer);
        let image = views
            .and_then(|v| v.first().copied())
            .and_then(|view| s.image_views.get(&view).copied());
        let rp = s.render_passes.get(&bi.render_pass);
        let clears = rp.map(|r| r.first_attachment_clears).unwrap_or(true);
        // Depth: the render pass's declared depth attachment picks the framebuffer's depth image view (by the
        // shared attachment index) and the pass's depth loadOp + clearValue — the classic-path mirror of the
        // dynamic-rendering pDepthAttachment. Its clearValue is read as depthStencil.depth (== float32[0]).
        let depth = rp.and_then(|r| r.depth).and_then(|d| {
            let image = views
                .and_then(|v| v.get(d.index as usize).copied())
                .and_then(|view| s.image_views.get(&view).copied())?;
            let clear_depth = clear_values.get(d.index as usize).map(|c| c.float32[0]).unwrap_or(1.0);
            Some(record::RenderingDepthAttachment { image, clear_depth, load_clear: d.clear })
        });
        let Some(image) = image else { return };
        if let Some(dev) = s.device.as_mut() {
            let _ = record::cmd_begin_render_pass(dev, cb, image, clear, clears, depth);
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

// ==================================================================================================
// dynamic rendering (VK_KHR_dynamic_rendering / core 1.3): render-pass-object-free recording
// ==================================================================================================

/// Resolve one `VkRenderingAttachmentInfo`'s `imageView` back to the `VkImage` handle it views (the hl
/// model renders into images directly). `None` on a null view / unmapped view (skipped as a no-attachment).
fn rendering_attachment_image(s: &crate::state::State, att: &VkRenderingAttachmentInfo) -> Option<u64> {
    s.image_views.get(&att.image_view).copied()
}

#[no_mangle]
pub extern "C" fn vkCmdBeginRendering(command_buffer: *mut c_void, p_rendering_info: *const c_void) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    let Some(ri) = (unsafe { (p_rendering_info as *const VkRenderingInfo).as_ref() }) else {
        return;
    };
    let colors_c: &[VkRenderingAttachmentInfo] =
        if ri.p_color_attachments.is_null() || ri.color_attachment_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(ri.p_color_attachments, ri.color_attachment_count as usize) }
        };
    let depth_c = unsafe { ri.p_depth_attachment.as_ref() };
    with(|s| {
        // Resolve each attachment view → image up front (image_views is disjoint from the device field).
        let colors: Vec<RenderingColorAttachment> = colors_c
            .iter()
            .filter_map(|att| {
                rendering_attachment_image(s, att).map(|image| RenderingColorAttachment {
                    image,
                    clear: att.clear_value.float32,
                    load_clear: att.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
                    store: att.store_op == VK_ATTACHMENT_STORE_OP_STORE,
                })
            })
            .collect();
        let depth = depth_c.and_then(|att| {
            rendering_attachment_image(s, att).map(|image| RenderingDepthAttachment {
                image,
                clear_depth: att.clear_value.float32[0],
                load_clear: att.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            })
        });
        if let Some(dev) = s.device.as_mut() {
            let _ = record::cmd_begin_rendering(dev, cb, &colors, depth);
        }
    });
}

/// `vkCmdBeginRenderingKHR` — the `VK_KHR_dynamic_rendering` alias of the promoted-core body.
#[no_mangle]
pub extern "C" fn vkCmdBeginRenderingKHR(command_buffer: *mut c_void, p_rendering_info: *const c_void) {
    vkCmdBeginRendering(command_buffer, p_rendering_info)
}

/// `vkCmdEndRendering` — close the dynamic-rendering pass (identical to `vkCmdEndRenderPass`:
/// `Enc::EndRenderPass`).
#[no_mangle]
pub extern "C" fn vkCmdEndRendering(command_buffer: *mut c_void) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|dev| {
        let _ = record::cmd_end_render_pass(dev, cb);
    });
}

/// `vkCmdEndRenderingKHR` — the `VK_KHR_dynamic_rendering` alias.
#[no_mangle]
pub extern "C" fn vkCmdEndRenderingKHR(command_buffer: *mut c_void) {
    vkCmdEndRendering(command_buffer)
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
// dynamic state (viewport / scissor lower to IR; the rest is recorded command state)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdSetViewport(
    command_buffer: *mut c_void,
    _first_viewport: u32,
    viewport_count: u32,
    p_viewports: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    if p_viewports.is_null() || viewport_count == 0 {
        return;
    }
    let v = unsafe { &*(p_viewports as *const VkViewport) };
    dev(|d| {
        let _ = record::cmd_set_viewport(d, cb, v.x, v.y, v.width, v.height, v.min_depth, v.max_depth);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetScissor(
    command_buffer: *mut c_void,
    _first_scissor: u32,
    scissor_count: u32,
    p_scissors: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    if p_scissors.is_null() || scissor_count == 0 {
        return;
    }
    let r = unsafe { &*(p_scissors as *const VkRect2D) };
    dev(|d| {
        let _ = record::cmd_set_scissor(
            d,
            cb,
            r.offset.x.max(0) as u32,
            r.offset.y.max(0) as u32,
            r.extent.width,
            r.extent.height,
        );
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetLineWidth(command_buffer: *mut c_void, line_width: f32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_set_line_width(d, cb, line_width);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetDepthBias(
    command_buffer: *mut c_void,
    depth_bias_constant_factor: f32,
    depth_bias_clamp: f32,
    depth_bias_slope_factor: f32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_set_depth_bias(
            d,
            cb,
            depth_bias_constant_factor,
            depth_bias_clamp,
            depth_bias_slope_factor,
        );
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetBlendConstants(command_buffer: *mut c_void, blend_constants: *const f32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    if blend_constants.is_null() {
        return;
    }
    let c = unsafe { std::slice::from_raw_parts(blend_constants, 4) };
    dev(|d| {
        let _ = record::cmd_set_blend_constants(d, cb, [c[0], c[1], c[2], c[3]]);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilCompareMask(command_buffer: *mut c_void, face_mask: u32, compare_mask: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_set_stencil_compare_mask(d, cb, face_mask, compare_mask);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilWriteMask(command_buffer: *mut c_void, face_mask: u32, write_mask: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_set_stencil_write_mask(d, cb, face_mask, write_mask);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdSetStencilReference(command_buffer: *mut c_void, face_mask: u32, reference: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_set_stencil_reference(d, cb, face_mask, reference);
    });
}

// ==================================================================================================
// push constants
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdPushConstants(
    command_buffer: *mut c_void,
    _layout: u64,
    _stage_flags: u32,
    offset: u32,
    size: u32,
    p_values: *const c_void,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    if p_values.is_null() || size == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p_values as *const u8, size as usize) };
    dev(|d| {
        let _ = record::cmd_push_constants(d, cb, offset, bytes);
    });
}

// ==================================================================================================
// indirect draws (validated; the IR carries no indirect draw op — a documented bring-up limit)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCmdDrawIndirect(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    draw_count: u32,
    stride: u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_draw_indirect(d, cb, buffer, offset, draw_count, stride);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdDrawIndexedIndirect(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    draw_count: u32,
    stride: u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev(|d| {
        let _ = record::cmd_draw_indexed_indirect(d, cb, buffer, offset, draw_count, stride);
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
    // then register the swapchain's presentable images against it. On success, carry the app's wayland
    // window (captured at `vkCreateWaylandSurfaceKHR` under `ci.surface`) onto the swapchain so a present
    // can marshal the readback onto the app's own `wl_surface`.
    with(|s| {
        let sink = &mut s.sink;
        let Some(dev) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let r = (|| {
            let surface = create_surface_for_swapchain(dev, sink, ci)?;
            present::create_swapchain(dev, sink, surface, ci.min_image_count)
        })();
        match r {
            Ok(h) => {
                if let Some(win) = s.wayland_surfaces.get(&ci.surface).copied() {
                    s.swapchain_windows.insert(h, win);
                }
                if !p_swapchain.is_null() {
                    unsafe { *p_swapchain = h };
                }
                VK_SUCCESS
            }
            Err(e) => vk_result_from_gpu_error(&e),
        }
    })
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
    with(|s| {
        let sink = &mut s.sink;
        if let Some(dev) = s.device.as_mut() {
            // Retire the swapchain AND its presentable images + presentation surface (dropping their
            // `dev.images`/`dev.surfaces` bookkeeping + freeing the host textures/surface). Removing only the
            // `SwapchainRec` would orphan the images in `dev.images` forever — a per-resize handle leak.
            let _ = present::destroy_swapchain(dev, sink, swapchain);
        }
        // Tear down the app-surface presenter + its window binding (drops the private queue wrappers +
        // the bound `wl_shm`, releasing the app's connection).
        s.swapchain_windows.remove(&swapchain);
        s.presenters.remove(&swapchain);
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
        // The swapchain's presentable images (real render-target textures + their VkImage handles) were
        // created with the swapchain; return the SAME handles here (identical on every call).
        let Ok(handles) = present::get_swapchain_images(dev, swapchain) else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let count = handles.len() as u32;
        if p_swapchain_images.is_null() {
            unsafe { *p_swapchain_image_count = count };
            return VK_SUCCESS;
        }
        let cap = unsafe { *p_swapchain_image_count };
        let n = cap.min(count);
        let out = unsafe { std::slice::from_raw_parts_mut(p_swapchain_images, n as usize) };
        for (slot, &handle) in out.iter_mut().zip(handles.iter()) {
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
        Err(e) => {
            hl_log::hl_warn!(tag::SHIM, "vkAcquireNextImageKHR sc={swapchain:#x} -> {:?}", vk_result_from_gpu_error(&e));
            vk_result_from_gpu_error(&e)
        }
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
    with(|s| {
        let sink = &mut s.sink;
        let Some(dev) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let mut res = VK_SUCCESS;
        for (&sc, &idx) in swapchains.iter().zip(indices) {
            // 1) The present lowering (`Cmd::Present` names the surface + presented image).
            if let Err(e) = present::queue_present(dev, sink, sc, idx) {
                res = vk_result_from_gpu_error(&e);
                continue;
            }
            // 2) Read the presented image back + convert to the XRGB plane a `wl_shm` buffer wants.
            let plane = match present::read_presented_xrgb(dev, sink, sc, idx) {
                Ok(p) => p,
                Err(e) => {
                    res = vk_result_from_gpu_error(&e);
                    continue;
                }
            };
            // 3) Marshal that plane onto the app's OWN `wl_surface` (soft-unavailable ⇒ readback-only,
            //    still VK_SUCCESS; a hard marshal/flush failure ⇒ VK_ERROR_OUT_OF_DATE/SURFACE_LOST).
            let vk = present_frame_to_app_surface(&mut s.presenters, &s.swapchain_windows, sc, plane);
            if vk != VK_SUCCESS {
                hl_log::hl_warn!(tag::PRESENT, "commit failed sc={sc:#x} -> {:?}", vk);
                res = vk;
            }
        }
        res
    })
}

/// Marshal one presented frame's XRGB plane onto the app's OWN `wl_surface` via a cached
/// [`WaylandAppPresenter`]. If the swapchain has no captured wayland window (a headless/offscreen or
/// non-wayland surface), the readback already ran and the on-surface attach is skipped — `VK_SUCCESS`. A
/// *soft* bring-up error (libwayland/global absent) caches `None` (so it is not re-probed each frame) and
/// is likewise `VK_SUCCESS`. A *hard* per-frame marshal/flush/size failure maps to
/// `VK_ERROR_OUT_OF_DATE_KHR` / `VK_ERROR_SURFACE_LOST_KHR` — never a faked present.
fn present_frame_to_app_surface(
    presenters: &mut std::collections::HashMap<u64, Option<WaylandAppPresenter>>,
    windows: &std::collections::HashMap<u64, WaylandWindow>,
    swapchain: u64,
    plane: (Vec<u8>, u32, u32),
) -> VkResult {
    let (xrgb, w, h) = plane;
    let Some(win) = windows.get(&swapchain) else {
        return VK_SUCCESS; // no captured wl_surface: readback-only present
    };
    // Bring the presenter up once, caching a soft-unavailable outcome as `None`.
    if !presenters.contains_key(&swapchain) {
        match WaylandAppPresenter::new(win.surface) {
            Ok(p) => {
                presenters.insert(swapchain, Some(p));
            }
            Err(e) if e.is_unavailable() => {
                presenters.insert(swapchain, None);
            }
            Err(e) => return e.to_vk_result(),
        }
    }
    match presenters.get_mut(&swapchain) {
        Some(Some(p)) => match p.present(&xrgb, w, h) {
            Ok(()) => VK_SUCCESS,
            Err(e) => e.to_vk_result(),
        },
        _ => VK_SUCCESS, // soft-unavailable: readback-only present
    }
}

// ==================================================================================================
// render pass 2 (VK_KHR_create_renderpass2 / core 1.2) — the `...2` create + begin/next/end aliases
// ==================================================================================================

/// `vkCreateRenderPass2` — the `VkRenderPassCreateInfo2` create form. Records the same single-target
/// bring-up bookkeeping (first color attachment's clear behaviour + format) as [`vkCreateRenderPass`],
/// reading the `VkAttachmentDescription2` attachment table.
#[no_mangle]
pub extern "C" fn vkCreateRenderPass2(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_render_pass: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkRenderPassCreateInfo2).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let (clears, fmt, depth) = if ci.p_attachments.is_null() || ci.attachment_count == 0 {
        (false, 0u32, None)
    } else {
        let atts = unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) };
        let a0 = &atts[0];
        let depth = atts.iter().enumerate().find(|(_, a)| is_depth_format(a.format as u32)).map(|(i, a)| {
            RenderPassDepth {
                index: i as u32,
                format_vk: a.format as u32,
                clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            }
        });
        (a0.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR, a0.format as u32, depth)
    };
    let handle = with(|s| {
        let h = s.device.as_mut()?.alloc_handle();
        s.render_passes
            .insert(h, RenderPassRec { first_attachment_clears: clears, color_format_vk: fmt, depth });
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

/// `vkCreateRenderPass2KHR` — the `VK_KHR_create_renderpass2` alias.
#[no_mangle]
pub extern "C" fn vkCreateRenderPass2KHR(
    device: *mut c_void,
    p_create_info: *const c_void,
    p_allocator: *const c_void,
    p_render_pass: *mut u64,
) -> VkResult {
    vkCreateRenderPass2(device, p_create_info, p_allocator, p_render_pass)
}

/// `vkCmdBeginRenderPass2` — the `VkRenderPassBeginInfo` is byte-identical to v1; the `VkSubpassBeginInfo`
/// only carries the (unmodeled) subpass-contents mode, so this delegates to [`vkCmdBeginRenderPass`].
#[no_mangle]
pub extern "C" fn vkCmdBeginRenderPass2(
    command_buffer: *mut c_void,
    p_render_pass_begin: *const c_void,
    _p_subpass_begin_info: *const c_void,
) {
    vkCmdBeginRenderPass(command_buffer, p_render_pass_begin, 0)
}

/// `vkCmdBeginRenderPass2KHR` — the `VK_KHR_create_renderpass2` alias.
#[no_mangle]
pub extern "C" fn vkCmdBeginRenderPass2KHR(
    command_buffer: *mut c_void,
    p_render_pass_begin: *const c_void,
    p_subpass_begin_info: *const c_void,
) {
    vkCmdBeginRenderPass2(command_buffer, p_render_pass_begin, p_subpass_begin_info)
}

/// `vkCmdEndRenderPass2` — delegates to [`vkCmdEndRenderPass`] (the `VkSubpassEndInfo` is unmodeled).
#[no_mangle]
pub extern "C" fn vkCmdEndRenderPass2(command_buffer: *mut c_void, _p_subpass_end_info: *const c_void) {
    vkCmdEndRenderPass(command_buffer)
}

/// `vkCmdEndRenderPass2KHR` — the `VK_KHR_create_renderpass2` alias.
#[no_mangle]
pub extern "C" fn vkCmdEndRenderPass2KHR(command_buffer: *mut c_void, p_subpass_end_info: *const c_void) {
    vkCmdEndRenderPass2(command_buffer, p_subpass_end_info)
}

/// `vkCmdNextSubpass` — advance to the next subpass. The bring-up render-pass model is single-subpass, so
/// this validates the command buffer and records nothing (a multi-subpass pass is not lowered).
#[no_mangle]
pub extern "C" fn vkCmdNextSubpass(command_buffer: *mut c_void, _contents: i32) {
    let _ = unsafe { cmdbuf_handle(command_buffer) };
}

/// `vkCmdNextSubpass2` — the `VkSubpassBeginInfo`/`VkSubpassEndInfo` form (single-subpass model no-op).
#[no_mangle]
pub extern "C" fn vkCmdNextSubpass2(
    command_buffer: *mut c_void,
    _p_subpass_begin_info: *const c_void,
    _p_subpass_end_info: *const c_void,
) {
    let _ = unsafe { cmdbuf_handle(command_buffer) };
}

/// `vkCmdNextSubpass2KHR` — the `VK_KHR_create_renderpass2` alias.
#[no_mangle]
pub extern "C" fn vkCmdNextSubpass2KHR(
    command_buffer: *mut c_void,
    p_subpass_begin_info: *const c_void,
    p_subpass_end_info: *const c_void,
) {
    vkCmdNextSubpass2(command_buffer, p_subpass_begin_info, p_subpass_end_info)
}

// Semaphores (binary present/acquire sync + timeline) are hand-written in `crate::sync`.

#[cfg(test)]
mod present_tests {
    use super::*;
    use std::collections::HashMap;

    /// A swapchain with NO captured wayland window (a headless/offscreen or non-wayland surface) still
    /// presents `VK_SUCCESS`: the readback already ran, the on-surface attach is simply skipped. The
    /// presenter cache stays empty (no bring-up attempted).
    #[test]
    fn no_wayland_window_is_readback_only_vk_success() {
        let mut presenters: HashMap<u64, Option<WaylandAppPresenter>> = HashMap::new();
        let windows: HashMap<u64, WaylandWindow> = HashMap::new();
        let plane = (vec![0xFFu8; 2 * 2 * 4], 2, 2);
        assert_eq!(present_frame_to_app_surface(&mut presenters, &windows, 0xABC, plane), VK_SUCCESS);
        assert!(presenters.is_empty(), "no window ⇒ no presenter bring-up");
    }

    /// A soft bring-up failure (here: a null `wl_surface*` ⇒ `WlAppError::NoSurface`, the same soft class
    /// as a missing `libwayland-client`) is cached as `None` and mapped to `VK_SUCCESS` — the readback-only
    /// present. A second frame reuses the cache (no re-probe) and is likewise `VK_SUCCESS`.
    #[test]
    fn soft_unavailable_bringup_caches_none_and_returns_vk_success() {
        let sc = 0xBEEFu64;
        let mut presenters: HashMap<u64, Option<WaylandAppPresenter>> = HashMap::new();
        let mut windows: HashMap<u64, WaylandWindow> = HashMap::new();
        // surface == 0 ⇒ WaylandAppPresenter::new short-circuits to NoSurface (soft) WITHOUT dlopen/deref.
        windows.insert(sc, WaylandWindow { display: 0xD15, surface: 0 });

        let vk = present_frame_to_app_surface(&mut presenters, &windows, sc, (vec![0xFFu8; 16], 2, 2));
        assert_eq!(vk, VK_SUCCESS, "soft-unavailable bring-up ⇒ readback-only VK_SUCCESS");
        assert!(matches!(presenters.get(&sc), Some(None)), "soft outcome must be cached as None");

        // Second frame: cache hit, still readback-only VK_SUCCESS.
        let vk2 = present_frame_to_app_surface(&mut presenters, &windows, sc, (vec![0xFFu8; 16], 2, 2));
        assert_eq!(vk2, VK_SUCCESS);
    }

    /// A pre-seeded soft-unavailable cache entry short-circuits to `VK_SUCCESS` (readback-only) — the
    /// steady-state path once `libwayland-client` was found absent.
    #[test]
    fn cached_soft_unavailable_is_vk_success() {
        let sc = 0x1234u64;
        let mut presenters: HashMap<u64, Option<WaylandAppPresenter>> = HashMap::new();
        presenters.insert(sc, None);
        let mut windows: HashMap<u64, WaylandWindow> = HashMap::new();
        windows.insert(sc, WaylandWindow { display: 0xD15, surface: 0xF00 });
        assert_eq!(
            present_frame_to_app_surface(&mut presenters, &windows, sc, (vec![0xFFu8; 16], 2, 2)),
            VK_SUCCESS
        );
    }
}
