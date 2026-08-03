use super::*;

/// `vkCmdBeginRenderPass` — begin a classic render pass targeting `color_image` with one color attachment
/// (`Clear` when `load_clear`, else `Load`; always stored), plus an optional `depth` attachment resolved
/// from the render pass's depth/stencil attachment + the framebuffer's bound depth image view. The depth
/// path is the exact mirror of the dynamic-rendering [`cmd_begin_rendering`] one — both emit the SAME
/// [`Enc::BeginRenderPass`] with a real [`DepthAttachment`], so a classic depth-tested pipeline occludes.
/// Ported from `command.rs::vkCmdBeginRenderPass`.
pub fn cmd_begin_render_pass(
    dev: &mut Device,
    cb: VkCommandBuffer,
    color_image: VkImage,
    clear: [f32; 4],
    load_clear: bool,
    depth: Option<RenderingDepthAttachment>,
) -> Result<()> {
    cmd_begin_render_pass_multi(
        dev,
        cb,
        &[RenderingColorAttachment {
            image: color_image,
            clear,
            load_clear,
            store: true,
        }],
        depth,
    )
}

/// Begin a classic render pass with every color attachment referenced by its first subpass.
pub fn cmd_begin_render_pass_multi(
    dev: &mut Device,
    cb: VkCommandBuffer,
    colors: &[RenderingColorAttachment],
    depth: Option<RenderingDepthAttachment>,
) -> Result<()> {
    let mut color_targets = Vec::with_capacity(colors.len());
    let mut extent = (0, 0);
    for color in colors {
        let image = dev.images.get(&color.image).ok_or(GpuError::Invalid(
            "vkCmdBeginRenderPass: unknown color VkImage",
        ))?;
        if extent == (0, 0) {
            extent = (image.width, image.height);
        }
        color_targets.push(ColorAttachment {
            texture: image.ir_id,
            load: if color.load_clear {
                LoadOp::Clear
            } else {
                LoadOp::Load
            },
            clear: color.clear.map(f64::from),
            store: color.store,
        });
    }
    // Resolve the depth image → ir texture id up front (a bad handle fails before recording), exactly as
    // the dynamic-rendering path does for its inline pDepthAttachment.
    let depth_target = match depth {
        Some(d) => {
            let depth_tex = dev
                .images
                .get(&d.image)
                .ok_or(GpuError::Invalid(
                    "vkCmdBeginRenderPass: unknown depth VkImage",
                ))?
                .ir_id;
            Some(DepthAttachment {
                texture: depth_tex,
                load: if d.load_clear {
                    LoadOp::Clear
                } else {
                    LoadOp::Load
                },
                clear_depth: d.clear_depth,
                clear_stencil: 0,
            })
        }
        None => None,
    };
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::BeginRenderPass {
        color: color_targets.clone(),
        depth: depth_target.clone(),
    });
    rec.replay_buffer_bindings();
    rec.in_render_pass = true;
    rec.active_pass = Some((color_targets.clone(), depth_target));
    rec.active_render_texture = color_targets.first().map(|color| color.texture);
    rec.render_extent = extent;
    rec.scissor = None;
    Ok(())
}

/// `vkCmdBindVertexBuffers` (one binding) — record `SetVertexBuffer`.
pub fn cmd_bind_vertex_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    slot: u32,
    buffer: VkBuffer,
    offset: u64,
) -> Result<()> {
    let ir = dev
        .buffers
        .get(&buffer)
        .ok_or(GpuError::Invalid(
            "vkCmdBindVertexBuffers: unknown VkBuffer",
        ))?
        .ir_id;
    let rec = dev.require_recording(cb)?;
    rec.bound_vertex_buffers.insert(slot, (ir, offset));
    rec.enc.push(Enc::SetVertexBuffer {
        slot,
        buffer: ir,
        offset,
    });
    Ok(())
}

/// `vkCmdBindIndexBuffer` — record `SetIndexBuffer` for the bound index buffer. `vk_index_type` is a raw
/// `VkIndexType` (`VK_INDEX_TYPE_UINT16` = 0, `VK_INDEX_TYPE_UINT32` = 1).
pub fn cmd_bind_index_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    buffer: VkBuffer,
    offset: u64,
    vk_index_type: u32,
) -> Result<()> {
    let ir = dev
        .buffers
        .get(&buffer)
        .ok_or(GpuError::Invalid("vkCmdBindIndexBuffer: unknown VkBuffer"))?
        .ir_id;
    let format = if vk_index_type == 1 {
        IndexFormat::U32
    } else {
        IndexFormat::U16
    };
    let rec = dev.require_recording(cb)?;
    rec.bound_index_buffer = Some((ir, offset, format));
    rec.enc.push(Enc::SetIndexBuffer {
        buffer: ir,
        offset,
        format,
    });
    Ok(())
}

/// `vkCmdDraw` — replay the bound pipeline + bind groups, then record `Draw`. Ported from
/// `command.rs::vkCmdDraw`.
pub fn cmd_draw(
    dev: &mut Device,
    cb: VkCommandBuffer,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) -> Result<()> {
    let rec = dev.require_recording(cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    if let Some(p) = pipeline {
        rec.enc.push(Enc::SetPipeline(p));
    }
    rec.replay_rebased_vertex_bindings()?;
    for (index, group) in groups {
        rec.enc.push(Enc::SetBindGroup { index, group });
    }
    rec.enc.push(Enc::Draw {
        vertex_count,
        instance_count,
        first_vertex,
        first_instance,
    });
    rec.accumulate_occlusion(instance_count);
    Ok(())
}

/// If an OCCLUSION query is open on this command buffer, add the draw's scissor-clipped sample footprint
/// to its running total (see [`CmdBufRec::occlusion_coverage`]).
impl CmdBufRec {
    /// Rebind for the current pipeline after moving its normalized attribute prefixes into Vulkan's
    /// buffer offsets. Pipeline and vertex-buffer bindings are independently dynamic state, so this is
    /// deliberately replayed immediately before every draw.
    pub(super) fn replay_rebased_vertex_bindings(&mut self) -> Result<()> {
        for (&slot, &(buffer, offset)) in &self.bound_vertex_buffers {
            let base = self
                .vertex_buffer_bases
                .get(slot as usize)
                .copied()
                .unwrap_or(0);
            if base == 0 {
                continue;
            }
            let offset = offset
                .checked_add(u64::from(base))
                .ok_or(GpuError::Invalid(
                    "vkCmdDraw: rebased vertex buffer offset overflow",
                ))?;
            self.enc.push(Enc::SetVertexBuffer {
                slot,
                buffer,
                offset,
            });
        }
        Ok(())
    }

    /// Replay Vulkan's persistent graphics buffer state into a newly opened neutral render pass.
    pub(super) fn replay_buffer_bindings(&mut self) {
        for (&slot, &(buffer, offset)) in &self.bound_vertex_buffers {
            self.enc.push(Enc::SetVertexBuffer {
                slot,
                buffer,
                offset,
            });
        }
        if let Some((buffer, offset, format)) = self.bound_index_buffer {
            self.enc.push(Enc::SetIndexBuffer {
                buffer,
                offset,
                format,
            });
        }
    }

    pub(super) fn accumulate_occlusion(&mut self, instance_count: u32) {
        if self.occlusion_accum.is_some() {
            let cov = self.occlusion_coverage(instance_count);
            if let Some(acc) = self.occlusion_accum.as_mut() {
                *acc = acc.saturating_add(cov);
            }
        }
    }
}

/// `vkCmdDrawIndexed` — replay the bound pipeline + bind groups, then record `DrawIndexed` against the
/// bound index buffer. Ported from `command.rs::vkCmdDrawIndexed`.
pub fn cmd_draw_indexed(
    dev: &mut Device,
    cb: VkCommandBuffer,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) -> Result<()> {
    let rec = dev.require_recording(cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    if let Some(p) = pipeline {
        rec.enc.push(Enc::SetPipeline(p));
    }
    rec.replay_rebased_vertex_bindings()?;
    for (index, group) in groups {
        rec.enc.push(Enc::SetBindGroup { index, group });
    }
    rec.enc.push(Enc::DrawIndexed {
        index_count,
        instance_count,
        first_index,
        base_vertex: vertex_offset,
        first_instance,
    });
    rec.accumulate_occlusion(instance_count);
    Ok(())
}

// ---- dynamic rendering (VK_KHR_dynamic_rendering / core 1.3) ------------------------------------
// A dynamic-rendering pass carries its attachments inline in `VkRenderingInfo` — no `VkRenderPass` or
// `VkFramebuffer` object. `vkCmdBeginRendering` lowers to the SAME `Enc::BeginRenderPass` a
// `vkCmdBeginRenderPass` does; the only difference is where the color/depth targets come from (the
// inline `pColorAttachments`/`pDepthAttachment`, resolved to image ir ids by the shim). `vkCmdEndRendering`
// is identical to `vkCmdEndRenderPass` (`Enc::EndRenderPass`), so it reuses [`cmd_end_render_pass`].

/// One parsed `VkRenderingAttachmentInfo` color target: the color `VkImage` (resolved from the
/// attachment's `imageView` by the shim), its clear value, and whether its `loadOp`/`storeOp` are
/// CLEAR/STORE. Neutral (no C ABI) so the lowering is unit-testable against a `RecordingSink`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderingColorAttachment {
    /// The color `VkImage` handle the attachment's `imageView` resolves to.
    pub image: VkImage,
    /// The RGBA clear value (`VkRenderingAttachmentInfo::clearValue.color`).
    pub clear: [f32; 4],
    /// `loadOp == VK_ATTACHMENT_LOAD_OP_CLEAR` (else the existing contents are loaded).
    pub load_clear: bool,
    /// `storeOp == VK_ATTACHMENT_STORE_OP_STORE` (else the result may be discarded).
    pub store: bool,
}

/// One parsed `VkRenderingAttachmentInfo` depth target.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderingDepthAttachment {
    /// The depth `VkImage` handle the attachment's `imageView` resolves to.
    pub image: VkImage,
    /// The depth clear value (`VkRenderingAttachmentInfo::clearValue.depthStencil.depth`).
    pub clear_depth: f32,
    /// `loadOp == VK_ATTACHMENT_LOAD_OP_CLEAR`.
    pub load_clear: bool,
}

/// `vkCmdBeginRendering(KHR)` — begin a render-pass-object-free pass from `VkRenderingInfo`: each color
/// attachment lowers to a [`ColorAttachment`] (`Clear` when its `loadOp` is CLEAR, else `Load`) and an
/// optional depth attachment to a [`DepthAttachment`], emitted as one [`Enc::BeginRenderPass`] — the SAME
/// op a classic render pass lowers to (§ dynamic rendering). Every attachment image must exist. The active
/// clear target (`vkCmdClearAttachments`) is the first color attachment.
pub fn cmd_begin_rendering(
    dev: &mut Device,
    cb: VkCommandBuffer,
    colors: &[RenderingColorAttachment],
    depth: Option<RenderingDepthAttachment>,
) -> Result<()> {
    // Resolve every attachment image → ir texture id up front (a bad handle fails before recording).
    let mut color_targets: Vec<ColorAttachment> = Vec::with_capacity(colors.len());
    let mut extent = (0u32, 0u32);
    for c in colors {
        let img = dev.images.get(&c.image).ok_or(GpuError::Invalid(
            "vkCmdBeginRendering: unknown color VkImage",
        ))?;
        if extent == (0, 0) {
            extent = (img.width, img.height);
        }
        color_targets.push(ColorAttachment {
            texture: img.ir_id,
            load: if c.load_clear {
                LoadOp::Clear
            } else {
                LoadOp::Load
            },
            clear: c.clear.map(f64::from),
            store: c.store,
        });
    }
    let depth_target = match depth {
        Some(d) => {
            let texture = dev
                .images
                .get(&d.image)
                .ok_or(GpuError::Invalid(
                    "vkCmdBeginRendering: unknown depth VkImage",
                ))?
                .ir_id;
            Some(DepthAttachment {
                texture,
                load: if d.load_clear {
                    LoadOp::Clear
                } else {
                    LoadOp::Load
                },
                clear_depth: d.clear_depth,
                clear_stencil: 0,
            })
        }
        None => None,
    };
    let active = color_targets.first().map(|c| c.texture);
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::BeginRenderPass {
        color: color_targets.clone(),
        depth: depth_target.clone(),
    });
    rec.replay_buffer_bindings();
    rec.in_render_pass = true;
    rec.active_pass = Some((color_targets, depth_target));
    rec.active_render_texture = active;
    rec.render_extent = extent;
    rec.scissor = None;
    Ok(())
}

/// `vkCmdEndRenderPass` — close the render pass.
impl Device {
    pub fn end_render_pass(&mut self, cb: VkCommandBuffer) -> Result<()> {
        let rec = self.require_recording(cb)?;
        rec.enc.push(Enc::EndRenderPass);
        rec.in_render_pass = false;
        rec.active_render_texture = None;
        rec.active_pass = None;
        Ok(())
    }
}
