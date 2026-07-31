use super::*;

// ==================================================================================================
// render pass + framebuffer (bring-up bookkeeping resolved at vkCmdBeginRenderPass)
// ==================================================================================================

/// Whether a raw `VkFormat` is a depth/stencil format — the contiguous `VK_FORMAT_D16_UNORM`(124) …
/// `VK_FORMAT_D32_SFLOAT_S8_UINT`(130) block (127 = `S8_UINT` is stencil-only, still a depth/stencil
/// attachment). Used to pick the depth attachment out of a classic render pass's attachment table.
struct AttachmentFormat;
impl AttachmentFormat {
    fn is_depth(f: u32) -> bool {
        (124..=130).contains(&f)
    }
}

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
        let atts =
            unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) };
        let a0 = &atts[0];
        let depth = atts
            .iter()
            .enumerate()
            .find(|(_, a)| AttachmentFormat::is_depth(a.format as u32))
            .map(|(i, a)| RenderPassDepth {
                index: i as u32,
                format_vk: a.format as u32,
                clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            });
        (
            a0.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            a0.format as u32,
            depth,
        )
    };
    let handle = StateStore::with(|s| {
        let h = s.device_mut()?.alloc_handle();
        s.render_passes.insert(
            h,
            RenderPassRec {
                first_attachment_clears: clears,
                color_format_vk: fmt,
                depth,
            },
        );
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

pub extern "C" fn vkDestroyRenderPass(
    _device: *mut c_void,
    render_pass: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.render_passes.remove(&render_pass);
    });
}

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
        unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) }
            .to_vec()
    };
    let handle = StateStore::with(|s| {
        let h = s.device_mut()?.alloc_handle();
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

pub extern "C" fn vkDestroyFramebuffer(
    _device: *mut c_void,
    framebuffer: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        s.framebuffers.remove(&framebuffer);
    });
}

// ==================================================================================================
// render pass 2 (VK_KHR_create_renderpass2 / core 1.2) — the `...2` create + begin/next/end aliases
// ==================================================================================================

/// `vkCreateRenderPass2` — the `VkRenderPassCreateInfo2` create form. Records the same single-target
/// bring-up bookkeeping (first color attachment's clear behaviour + format) as [`vkCreateRenderPass`],
/// reading the `VkAttachmentDescription2` attachment table.
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
        let atts =
            unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) };
        let a0 = &atts[0];
        let depth = atts
            .iter()
            .enumerate()
            .find(|(_, a)| AttachmentFormat::is_depth(a.format as u32))
            .map(|(i, a)| RenderPassDepth {
                index: i as u32,
                format_vk: a.format as u32,
                clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            });
        (
            a0.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            a0.format as u32,
            depth,
        )
    };
    let handle = StateStore::with(|s| {
        let h = s.device_mut()?.alloc_handle();
        s.render_passes.insert(
            h,
            RenderPassRec {
                first_attachment_clears: clears,
                color_format_vk: fmt,
                depth,
            },
        );
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
pub extern "C" fn vkCmdBeginRenderPass2(
    command_buffer: *mut c_void,
    p_render_pass_begin: *const c_void,
    _p_subpass_begin_info: *const c_void,
) {
    vkCmdBeginRenderPass(command_buffer, p_render_pass_begin, 0)
}

/// `vkCmdBeginRenderPass2KHR` — the `VK_KHR_create_renderpass2` alias.
pub extern "C" fn vkCmdBeginRenderPass2KHR(
    command_buffer: *mut c_void,
    p_render_pass_begin: *const c_void,
    p_subpass_begin_info: *const c_void,
) {
    vkCmdBeginRenderPass2(command_buffer, p_render_pass_begin, p_subpass_begin_info)
}

/// `vkCmdEndRenderPass2` — delegates to [`vkCmdEndRenderPass`] (the `VkSubpassEndInfo` is unmodeled).
pub extern "C" fn vkCmdEndRenderPass2(
    command_buffer: *mut c_void,
    _p_subpass_end_info: *const c_void,
) {
    vkCmdEndRenderPass(command_buffer)
}

/// `vkCmdEndRenderPass2KHR` — the `VK_KHR_create_renderpass2` alias.
pub extern "C" fn vkCmdEndRenderPass2KHR(
    command_buffer: *mut c_void,
    p_subpass_end_info: *const c_void,
) {
    vkCmdEndRenderPass2(command_buffer, p_subpass_end_info)
}

/// `vkCmdNextSubpass` — advance to the next subpass. The bring-up render-pass model is single-subpass, so
/// this validates the command buffer and records nothing (a multi-subpass pass is not lowered).
pub extern "C" fn vkCmdNextSubpass(command_buffer: *mut c_void, _contents: i32) {
    let _ = unsafe { CommandBuffer::handle(command_buffer) };
}

/// `vkCmdNextSubpass2` — the `VkSubpassBeginInfo`/`VkSubpassEndInfo` form (single-subpass model no-op).
pub extern "C" fn vkCmdNextSubpass2(
    command_buffer: *mut c_void,
    _p_subpass_begin_info: *const c_void,
    _p_subpass_end_info: *const c_void,
) {
    let _ = unsafe { CommandBuffer::handle(command_buffer) };
}

/// `vkCmdNextSubpass2KHR` — the `VK_KHR_create_renderpass2` alias.
pub extern "C" fn vkCmdNextSubpass2KHR(
    command_buffer: *mut c_void,
    p_subpass_begin_info: *const c_void,
    p_subpass_end_info: *const c_void,
) {
    vkCmdNextSubpass2(command_buffer, p_subpass_begin_info, p_subpass_end_info)
}

// Semaphores (binary present/acquire sync + timeline) are hand-written in `crate::sync`.
