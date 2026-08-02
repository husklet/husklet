use super::*;

// ==================================================================================================
// render pass + framebuffer (bring-up bookkeeping resolved at vkCmdBeginRenderPass)
// ==================================================================================================

const VK_ATTACHMENT_UNUSED: u32 = u32::MAX;

pub extern "C" fn vkCreateRenderPass(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_render_pass: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkRenderPassCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let (colors, depth) = if ci.p_attachments.is_null()
        || ci.attachment_count == 0
        || ci.p_subpasses.is_null()
        || ci.subpass_count == 0
    {
        (Vec::new(), None)
    } else {
        let atts =
            unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) };
        let subpass = unsafe { &*(ci.p_subpasses as *const VkSubpassDescription) };
        let refs = if subpass.p_color_attachments.is_null() {
            &[][..]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    subpass.p_color_attachments,
                    subpass.color_attachment_count as usize,
                )
            }
        };
        let colors = refs
            .iter()
            .filter(|r| r.attachment != VK_ATTACHMENT_UNUSED)
            .filter_map(|r| {
                let a = atts.get(r.attachment as usize)?;
                Some(RenderPassColor {
                    index: r.attachment,
                    format_vk: a.format as u32,
                    clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
                })
            })
            .collect();
        let depth = unsafe { subpass.p_depth_stencil_attachment.as_ref() }
            .filter(|r| r.attachment != VK_ATTACHMENT_UNUSED)
            .and_then(|r| atts.get(r.attachment as usize).map(|a| (r, a)))
            .map(|(r, a)| RenderPassDepth {
                index: r.attachment,
                format_vk: a.format as u32,
                clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            });
        (colors, depth)
    };
    let handle = StateStore::with(|s| {
        let h = s.device_mut()?.alloc_handle();
        s.render_passes.insert(
            h,
            RenderPassRec {
                colors,
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
    let (colors, depth) = if ci.p_attachments.is_null()
        || ci.attachment_count == 0
        || ci.p_subpasses.is_null()
        || ci.subpass_count == 0
    {
        (Vec::new(), None)
    } else {
        let atts =
            unsafe { std::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) };
        let subpass = unsafe { &*(ci.p_subpasses as *const VkSubpassDescription2) };
        let refs = if subpass.p_color_attachments.is_null() {
            &[][..]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    subpass.p_color_attachments,
                    subpass.color_attachment_count as usize,
                )
            }
        };
        let colors = refs
            .iter()
            .filter(|r| r.attachment != VK_ATTACHMENT_UNUSED)
            .filter_map(|r| {
                let a = atts.get(r.attachment as usize)?;
                Some(RenderPassColor {
                    index: r.attachment,
                    format_vk: a.format as u32,
                    clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
                })
            })
            .collect();
        let depth = unsafe { subpass.p_depth_stencil_attachment.as_ref() }
            .filter(|r| r.attachment != VK_ATTACHMENT_UNUSED)
            .and_then(|r| atts.get(r.attachment as usize).map(|a| (r, a)))
            .map(|(r, a)| RenderPassDepth {
                index: r.attachment,
                format_vk: a.format as u32,
                clear: a.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
            });
        (colors, depth)
    };
    let handle = StateStore::with(|s| {
        let h = s.device_mut()?.alloc_handle();
        s.render_passes.insert(
            h,
            RenderPassRec {
                colors,
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
