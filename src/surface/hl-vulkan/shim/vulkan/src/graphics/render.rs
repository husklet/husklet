use super::*;

// ==================================================================================================
// render-pass command recording
// ==================================================================================================

pub extern "C" fn vkCmdBeginRenderPass(
    command_buffer: *mut c_void,
    p_render_pass_begin: *const c_void,
    _contents: i32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(bi) = (unsafe { (p_render_pass_begin as *const VkRenderPassBeginInfo).as_ref() })
    else {
        return;
    };
    // The clear color is the first pClearValues entry (color aspect); default opaque black.
    let clear = if bi.p_clear_values.is_null() || bi.clear_value_count == 0 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        unsafe { (*bi.p_clear_values).float32 }
    };
    // The per-attachment clear values are indexed by attachment slot (color at 0, depth at its own slot).
    let clear_values: &[VkClearValue] = if bi.p_clear_values.is_null() || bi.clear_value_count == 0
    {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bi.p_clear_values, bi.clear_value_count as usize) }
    };
    StateStore::with(|s| {
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
            let clear_depth = clear_values
                .get(d.index as usize)
                .map(|c| c.float32[0])
                .unwrap_or(1.0);
            Some(record::RenderingDepthAttachment {
                image,
                clear_depth,
                load_clear: d.clear,
            })
        });
        let Some(image) = image else { return };
        if let Some(dev) = s.device.as_mut() {
            let _ = record::cmd_begin_render_pass(dev, cb, image, clear, clears, depth);
        }
    });
}

pub extern "C" fn vkCmdEndRenderPass(command_buffer: *mut c_void) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|dev| {
        let _ = dev.end_render_pass(cb);
    });
}

// ==================================================================================================
// dynamic rendering (VK_KHR_dynamic_rendering / core 1.3): render-pass-object-free recording
// ==================================================================================================

/// Resolve one `VkRenderingAttachmentInfo`'s `imageView` back to the `VkImage` handle it views (the hl
/// model renders into images directly). `None` on a null view / unmapped view (skipped as a no-attachment).
struct RenderingAttachment;
impl RenderingAttachment {
    fn image(s: &crate::state::State, att: &VkRenderingAttachmentInfo) -> Option<u64> {
        s.image_views.get(&att.image_view).copied()
    }
}

pub extern "C" fn vkCmdBeginRendering(
    command_buffer: *mut c_void,
    p_rendering_info: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(ri) = (unsafe { (p_rendering_info as *const VkRenderingInfo).as_ref() }) else {
        return;
    };
    let colors_c: &[VkRenderingAttachmentInfo] = if ri.p_color_attachments.is_null()
        || ri.color_attachment_count == 0
    {
        &[]
    } else {
        unsafe {
            std::slice::from_raw_parts(ri.p_color_attachments, ri.color_attachment_count as usize)
        }
    };
    let depth_c = unsafe { ri.p_depth_attachment.as_ref() };
    StateStore::with(|s| {
        // Resolve each attachment view → image up front (image_views is disjoint from the device field).
        let colors: Vec<RenderingColorAttachment> = colors_c
            .iter()
            .filter_map(|att| {
                RenderingAttachment::image(s, att).map(|image| RenderingColorAttachment {
                    image,
                    clear: att.clear_value.float32,
                    load_clear: att.load_op == VK_ATTACHMENT_LOAD_OP_CLEAR,
                    store: att.store_op == VK_ATTACHMENT_STORE_OP_STORE,
                })
            })
            .collect();
        let depth = depth_c.and_then(|att| {
            RenderingAttachment::image(s, att).map(|image| RenderingDepthAttachment {
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
pub extern "C" fn vkCmdBeginRenderingKHR(
    command_buffer: *mut c_void,
    p_rendering_info: *const c_void,
) {
    vkCmdBeginRendering(command_buffer, p_rendering_info)
}

/// `vkCmdEndRendering` — close the dynamic-rendering pass (identical to `vkCmdEndRenderPass`:
/// `Enc::EndRenderPass`).
pub extern "C" fn vkCmdEndRendering(command_buffer: *mut c_void) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|dev| {
        let _ = dev.end_render_pass(cb);
    });
}

/// `vkCmdEndRenderingKHR` — the `VK_KHR_dynamic_rendering` alias.
pub extern "C" fn vkCmdEndRenderingKHR(command_buffer: *mut c_void) {
    vkCmdEndRendering(command_buffer)
}

pub extern "C" fn vkCmdBindVertexBuffers(
    command_buffer: *mut c_void,
    first_binding: u32,
    binding_count: u32,
    p_buffers: *const u64,
    p_offsets: *const u64,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
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
    ShimState::with_device(|dev| {
        for (i, &buf) in buffers.iter().enumerate() {
            let slot = first_binding + i as u32;
            let offset = offsets.get(i).copied().unwrap_or(0);
            let _ = record::cmd_bind_vertex_buffer(dev, cb, slot, buf, offset);
        }
    });
}

pub extern "C" fn vkCmdBindIndexBuffer(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    index_type: i32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|dev| {
        let _ = record::cmd_bind_index_buffer(dev, cb, buffer, offset, index_type as u32);
    });
}

pub extern "C" fn vkCmdDraw(
    command_buffer: *mut c_void,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|dev| {
        let _ = record::cmd_draw(
            dev,
            cb,
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        );
    });
}

pub extern "C" fn vkCmdDrawIndexed(
    command_buffer: *mut c_void,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|dev| {
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

/// `vkCmdBindIndexBuffer2` (core 1.4, promoted from `VK_KHR_maintenance5`) is `vkCmdBindIndexBuffer`
/// plus an explicit `size`. This backend binds the index buffer whole and derives the index count from
/// each draw, so `size` adds no state it can honour and the binding itself is identical — it forwards
/// to the same `record::cmd_bind_index_buffer` lowering. It was previously a silent no-op, so an
/// application using the 1.4 spelling drew with no index buffer bound and got wrong geometry with no
/// error.
pub extern "C" fn vkCmdBindIndexBuffer2(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    _size: u64,
    index_type: i32,
) {
    vkCmdBindIndexBuffer(command_buffer, buffer, offset, index_type)
}

/// `vkCmdBindIndexBuffer2KHR` — the pre-promotion `VK_KHR_maintenance5` spelling of the same command.
pub extern "C" fn vkCmdBindIndexBuffer2KHR(
    command_buffer: *mut c_void,
    buffer: u64,
    offset: u64,
    size: u64,
    index_type: i32,
) {
    vkCmdBindIndexBuffer2(command_buffer, buffer, offset, size, index_type)
}
