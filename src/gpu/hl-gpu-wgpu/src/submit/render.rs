use super::*;

mod plan;

/// Whether this texture (or explicit view) can actually be BOUND as a render-pass attachment.
///
/// Distinct from `render_attachment`, which records only that the usage was granted. A pass binds the
/// default view, and wgpu requires an attachment view to name exactly one mip level and one array layer
/// of a 2D image; a texture that is layered, multi-level, 1D or 3D is refused here rather than at
/// `RenderPass::end`, where the message names the view it was handed instead of what the caller passed.
fn bindable_as_attachment(t: &texture::WgpuTexture) -> bool {
    t.dim == hl_gpu::protocol::model::enums::TextureDim::D2 && t.depth == 1 && t.mip_levels == 1
}

impl WgpuExecutor {
    /// Execute one render pass: begin with the color attachments (clear/load), replay any pipeline/bind/
    /// draw ops into it, end, submit, and wait. A clear-only pass (no draws) realizes the clear.
    ///
    /// Bind groups are honored PER SET INDEX (like `run_compute_pass`): each draw binds every set the stream
    /// currently has bound, each built against THAT set's own layout on the bound pipeline
    /// (`get_bind_group_layout(index)`) and bound at its declared index. This is what lets a pipeline whose
    /// vertex/fragment read bindings in group 0 AND group 1 (Zed's GPUI draws) match — a set-1 bind group is
    /// validated against group 1's layout, not group 0's. A draw with no bind group (the bindingless
    /// conformance triangle) stays the fast path — unlike compute, an empty group list is legal.
    pub(super) fn run_render_pass(
        &mut self,
        res: &SessionResources,
        color: &[ColorAttachment],
        depth: Option<&DepthAttachment>,
        ops: &[Enc],
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        // Run the pass under a validation scope so a wgpu rejection at pass-end/submit (e.g. a draw whose
        // vertex/index count overruns the bound buffer, or a depth-tested pipeline drawn with no depth
        // attachment) is a typed error, not the panicking default handler — and the executor survives.
        let started = self.profile_clock();
        let result = self
            .with_validation_scope(|s| s.run_render_pass_inner(res, color, depth, ops, encoder));
        self.profile_record(|p| &mut p.render_passes, started);
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_render_pass_inner(
        &mut self,
        res: &SessionResources,
        color: &[ColorAttachment],
        depth: Option<&DepthAttachment>,
        ops: &[Enc],
        enc: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::EXEC, "submit");
        hl_log::hl_count!(tag::EXEC, "passes");
        // Resolve attachment views up front (they must outlive the pass). While resolving, track the render
        // pass's render extent (`target_w`/`target_h`) — the MIN width/height across all attachments, which
        // is exactly the bound wgpu validates a viewport against (`state.info.extent`). It is the clip
        // rectangle a GL-style out-of-bounds viewport must be intersected with (see `clamp_viewport`).
        let mut views = Vec::with_capacity(color.len());
        let mut target_formats = Vec::with_capacity(color.len());
        let mut target_w = u32::MAX;
        let mut target_h = u32::MAX;
        for c in color {
            let t = texture::WgpuTexture::get(res, c.texture)?;
            // A texture that was not created with RENDER_ATTACHMENT cannot be a colour target, and saying
            // so HERE is the whole point: without this the command reaches wgpu and fails at
            // `RenderPass::end` with a message about the shape rather than about the mistake. Measured, a
            // 2D-array texture named as a colour attachment reported
            // `MissingFeatures(MULTIVIEW)` — a feature nobody asked for, which sends the reader to
            // multiview support instead of to "this texture is not a render target". The predicate is the
            // creation-time grant itself (`WgpuTexture::render_attachment`), not a re-derived shape rule,
            // so the guard cannot drift from the thing it guards.
            if !t.render_attachment {
                return Err(GpuError::Invalid(
                    "wgpu: colour attachment texture was not created as a render target",
                ));
            }
            // The grant says "this may be a render target"; it does not say "this VIEW can be bound as
            // one", and the two came apart on a multi-mip texture. An attachment binds the texture's
            // default view, which spans every level it has, and wgpu requires an attachment view to name
            // exactly one — measured, a three-level texture produced
            // `TextureViewIsNotRenderable { reason: MipLevelCount(3) }` at `RenderPass::end`.
            //
            // This is the predicate the grant/guard coupling test's own failure message prescribes: the
            // bound view must be a single-layer, single-mip 2D view, because that is what a colour pass
            // can target. An explicit `CreateTextureView` that selects one level of a multi-level
            // texture satisfies it and is deliberately still allowed.
            if !bindable_as_attachment(t) {
                return Err(GpuError::Invalid(
                    "wgpu: colour attachment must be a single-layer, single-mip 2D view",
                ));
            }
            target_w = target_w.min(t.width);
            target_h = target_h.min(t.height);
            target_formats.push(t.format);
            views.push((t.view.clone(), c));
        }
        // Resolve the depth attachment's view + load/clear (it too must outlive the pass). A pipeline
        // built with a depth-stencil state MUST run in a pass with a matching depth attachment (wgpu
        // enforces this), so honoring the attachment here is what makes depth-tested draws real instead
        // of silently dropping the depth field.
        // A stencil aspect exists only for a combined depth+stencil format; for a depth-only attachment the
        // pass MUST NOT carry stencil ops (wgpu rejects `stencil_ops: Some(_)` on a `Depth32Float` view).
        // So the attachment's stencil clear/store is honored precisely when the format has that aspect.
        let depth_view = match depth {
            Some(d) => {
                let t = texture::WgpuTexture::get(res, d.texture)?;
                if !t.render_attachment {
                    return Err(GpuError::Invalid(
                        "wgpu: depth attachment texture was not created as a render target",
                    ));
                }
                if !bindable_as_attachment(t) {
                    return Err(GpuError::Invalid(
                        "wgpu: depth attachment must be a single-layer, single-mip 2D view",
                    ));
                }
                // The depth attachment MUST be a depth(+stencil) format: a color-format texture handed as a
                // `depth_stencil_attachment` is a hard wgpu validation error (its handler panics). The
                // runtime does not type-check the attachment, so reject a non-depth format here.
                if !matches!(
                    t.format,
                    TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8
                ) {
                    return Err(GpuError::Invalid(
                        "wgpu: depth attachment is not a depth format",
                    ));
                }
                let has_stencil = matches!(t.format, TextureFormat::Depth24PlusStencil8);
                // The depth attachment participates in the render extent wgpu validates against.
                target_w = target_w.min(t.width);
                target_h = target_h.min(t.height);
                Some((
                    t.view.clone(),
                    d.load,
                    d.clear_depth,
                    d.clear_stencil,
                    has_stencil,
                ))
            }
            None => None,
        };
        // Every bind group a draw binds must exist BEFORE the pass opens (an open pass borrows the
        // encoder), so the whole draw plan — viewport-correction uniform + one concrete bind group per
        // (draw, set), memoized — is built first. See `plan`.
        let plan = plan::Plan::build(self, res, ops, &target_formats, target_w, target_h)?;
        let draws = &plan.draws;

        {
            let attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = views
                .iter()
                .map(|(view, c)| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: match c.load {
                                LoadOp::Clear => wgpu::LoadOp::Clear(wgpu::Color {
                                    r: c.clear[0],
                                    g: c.clear[1],
                                    b: c.clear[2],
                                    a: c.clear[3],
                                }),
                                _ => wgpu::LoadOp::Load,
                            },
                            // Honor the attachment's `store` flag: `true` (the common case) keeps the drawn
                            // result so a later readback/present sees it; `false` is WebGPU `StoreOp::Discard`
                            // — the guest declared it will not read this target, so its contents become
                            // undefined after the pass. Previously this was hardcoded to `Store`, silently
                            // ignoring the wire field (the same class of dropped-field bug the #209 audit
                            // found for write_mask/cull/front_face).
                            store: if c.store {
                                wgpu::StoreOp::Store
                            } else {
                                wgpu::StoreOp::Discard
                            },
                        },
                    })
                })
                .collect();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-render-pass"),
                color_attachments: &attachments,
                depth_stencil_attachment: depth_view.as_ref().map(
                    |(view, load, clear, clear_stencil, has_stencil)| {
                        wgpu::RenderPassDepthStencilAttachment {
                            view,
                            depth_ops: Some(wgpu::Operations {
                                load: match load {
                                    LoadOp::Clear => wgpu::LoadOp::Clear(*clear),
                                    _ => wgpu::LoadOp::Load,
                                },
                                store: wgpu::StoreOp::Store,
                            }),
                            // Honor the stencil clear/store only for a format that actually has a stencil
                            // aspect; `LoadOp::Load` preserves a stencil written by an earlier pass (what a
                            // two-pass stencil-mask-then-test IR relies on).
                            stencil_ops: has_stencil.then(|| wgpu::Operations {
                                load: match load {
                                    LoadOp::Clear => wgpu::LoadOp::Clear(*clear_stencil),
                                    _ => wgpu::LoadOp::Load,
                                },
                                store: wgpu::StoreOp::Store,
                            }),
                        }
                    },
                ),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut di = 0usize;
            // Tracks whether the CURRENTLY-bound viewport intersected the render target to an EMPTY rect
            // (the whole GL viewport lies outside the attachment). wgpu has no way to express such a viewport,
            // and GL would rasterize nothing through it, so draws issued while this is set are dropped (they
            // produce no pixels — exactly GL's result) instead of falling back to wgpu's default full-target
            // viewport, which would wrongly paint the geometry. Reset by the next in-bounds SetViewport.
            let mut vp_degenerate = false;
            // The same for a scissor rect whose intersection with the target is empty (see `Scissor`).
            let mut sc_degenerate = false;
            // Exact logical buffer bindings seen by wgpu. Kept beside the native pass state solely so a
            // validation failure can name the offending pipeline/slot/buffer tuple.
            let mut vertex_bindings = std::collections::BTreeMap::new();
            for op in ops {
                match op {
                    Enc::SetViewport {
                        x,
                        y,
                        w,
                        h,
                        min_depth,
                        max_depth,
                    } => {
                        // GL's glViewport permits a rect that starts negative or extends past the framebuffer
                        // — GL treats the viewport purely as the NDC→window transform and lets the framebuffer
                        // clip fragments. wgpu's `set_viewport` instead REJECTS any rect not fully inside the
                        // attachment (`x,y>=0`, `x+w<=tw`, `y+h<=th`, `w,h>0`) as a device-validation error,
                        // which is what NACKed Chrome's first real frame (a scrolled layer sends e.g.
                        // `y=-386, h=642` into a 256-tall target) and orphaned its pass resources downstream.
                        // Intersect the GL rect with the render target so wgpu accepts it; drop the draws when
                        // the intersection is empty. See `clamp_viewport` for the fidelity note.
                        match Viewport::new(*x, *y, *w, *h, target_w, target_h) {
                            Some(viewport) => {
                                vp_degenerate = false;
                                let (cx, cy, cw, ch) = viewport.clipped;
                                // Depth range is clamped into [0,1] so a stray out-of-range depth (which wgpu
                                // also rejects) can't reintroduce the very NACK we're clearing.
                                pass.set_viewport(
                                    cx,
                                    cy,
                                    cw,
                                    ch,
                                    min_depth.clamp(0.0, 1.0),
                                    max_depth.clamp(0.0, 1.0),
                                );
                            }
                            None => vp_degenerate = true,
                        }
                    }
                    // GL's glScissor is clipped by the framebuffer; wgpu rejects an overhanging rect
                    // outright (and adds `x + w` with wrapping arithmetic). Intersect it, and drop the
                    // draws when the intersection is empty — GL rasterizes nothing through such a rect.
                    Enc::SetScissor { x, y, w, h } => {
                        match Scissor::new(*x, *y, *w, *h, target_w, target_h) {
                            Some(scissor) => {
                                sc_degenerate = false;
                                let (sx, sy, sw, sh) = scissor.clipped;
                                pass.set_scissor_rect(sx, sy, sw, sh);
                            }
                            None => sc_degenerate = true,
                        }
                    }
                    // Dynamic stencil reference: the value the pipeline's stencil compare tests against and
                    // that a `REPLACE` op writes. Applied in stream order, so it takes effect for the draws
                    // that follow it — exactly like viewport/scissor.
                    Enc::SetStencilReference { reference } => {
                        pass.set_stencil_reference(*reference)
                    }
                    Enc::SetBlendConstant { color } => pass.set_blend_constant(wgpu::Color {
                        r: color[0] as f64,
                        g: color[1] as f64,
                        b: color[2] as f64,
                        a: color[3] as f64,
                    }),
                    Enc::SetVertexBuffer {
                        slot,
                        buffer,
                        offset,
                    } => {
                        let vb = buffer::WgpuBuffer::get(res, *buffer)?;
                        // `Buffer::slice(offset..)` PANICS (a hard Rust assert, not a routable wgpu error)
                        // when `offset` runs past the buffer, so a hostile offset must be rejected here.
                        if *offset > vb.size {
                            return Err(GpuError::OutOfBounds);
                        }
                        vertex_bindings.insert(
                            *slot,
                            vertex::Binding {
                                buffer: *buffer,
                                size: vb.size,
                                offset: *offset,
                            },
                        );
                        pass.set_vertex_buffer(*slot, vb.buffer.slice(*offset..));
                    }
                    Enc::SetIndexBuffer {
                        buffer,
                        offset,
                        format,
                    } => {
                        let ib = buffer::WgpuBuffer::get(res, *buffer)?;
                        if *offset > ib.size {
                            return Err(GpuError::OutOfBounds);
                        }
                        let format = match format {
                            IndexFormat::U16 => wgpu::IndexFormat::Uint16,
                            IndexFormat::U32 => wgpu::IndexFormat::Uint32,
                        };
                        pass.set_index_buffer(ib.buffer.slice(*offset..), format);
                    }
                    Enc::Draw {
                        vertex_count,
                        instance_count,
                        first_vertex,
                        first_instance,
                    } => {
                        let planned = &draws[di];
                        let pid = &planned.id;
                        di += 1;
                        let layouts = match PipelineNative::get(res, *pid)? {
                            PipelineNative::Render {
                                vertex_buffers,
                                ..
                            } => vertex_buffers,
                            PipelineNative::Compute { .. } => {
                                unreachable!("draw pipeline checked above")
                            }
                        };
                        pass.set_pipeline(&planned.pipeline);
                        for b in &planned.groups {
                            match b.offset {
                                Some(offset) => pass.set_bind_group(b.index, &b.group, &[offset]),
                                None => pass.set_bind_group(b.index, &b.group, &[]),
                            }
                        }
                        // Build the vertex/instance ranges with wrapping-safe arithmetic: a hostile
                        // `first_vertex + vertex_count` (or instance) that overflows `u32` is a debug panic
                        // here, before wgpu ever sees it. The runtime does not range-check Draw.
                        let v_end = first_vertex
                            .checked_add(*vertex_count)
                            .ok_or(GpuError::Invalid("wgpu: draw vertex range overflow"))?;
                        let i_end = first_instance
                            .checked_add(*instance_count)
                            .ok_or(GpuError::Invalid("wgpu: draw instance range overflow"))?;
                        vertex::Draw {
                            pipeline: *pid,
                            layouts,
                            bindings: &vertex_bindings,
                            first_vertex: *first_vertex,
                            vertex_count: *vertex_count,
                            first_instance: *first_instance,
                            instance_count: *instance_count,
                        }
                        .log();
                        // A draw under a wholly-out-of-bounds viewport or an empty scissor rasterizes
                        // nothing in GL; skip it (di/pipeline/binds already advanced) rather than draw
                        // through wgpu's default full-target viewport/scissor.
                        if !vp_degenerate && !sc_degenerate {
                            pass.draw(*first_vertex..v_end, *first_instance..i_end);
                        }
                    }
                    Enc::DrawIndexed {
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                    } => {
                        let planned = &draws[di];
                        di += 1;
                        pass.set_pipeline(&planned.pipeline);
                        for b in &planned.groups {
                            match b.offset {
                                Some(offset) => pass.set_bind_group(b.index, &b.group, &[offset]),
                                None => pass.set_bind_group(b.index, &b.group, &[]),
                            }
                        }
                        let idx_end = first_index
                            .checked_add(*index_count)
                            .ok_or(GpuError::Invalid("wgpu: draw index range overflow"))?;
                        let i_end = first_instance
                            .checked_add(*instance_count)
                            .ok_or(GpuError::Invalid("wgpu: draw instance range overflow"))?;
                        // See the `Draw` arm: a wholly-out-of-bounds viewport or empty scissor draws
                        // nothing in GL.
                        if !vp_degenerate && !sc_degenerate {
                            pass.draw_indexed(
                                *first_index..idx_end,
                                *base_vertex,
                                *first_instance..i_end,
                            );
                        }
                    }
                    // Consumed by the PLAN pass (`plan.rs`), which resolves the pipeline and bind groups
                    // before this loop runs. Seeing them here again is expected and correct.
                    Enc::SetPipeline(_) | Enc::SetBindGroup { .. } => {}
                    // Anything else inside a render pass is work this executor cannot perform HERE, and
                    // silently ignoring it is the failure mode that matters: a `ClearRect`, copy, blit,
                    // resolve or fill encoded between Begin and End was dropped and the submit reported
                    // success, so a region of a surface the guest asked to have written simply stayed as
                    // it was minted. The CPU oracle executes those ops from the same position (its loop is
                    // flat and state-driven), so the two executors silently disagreed — which no test
                    // caught, because none of them puts such an op inside a pass.
                    //
                    // Refusing makes the disagreement visible rather than invisible. It is deliberately
                    // not the last word: either the oracle should refuse too, or this path should split
                    // the pass and perform the work. A NACK is the honest state until that is decided.
                    other => {
                        // A VERDICT: the guest asked for this work and it will not happen, and the ack
                        // byte the guest receives cannot say which op it was. Previously `hl_warn!`,
                        // which compiles out of release entirely — the build where this matters — and
                        // was masked behind a tag nobody had opened even in debug.
                        hl_log::hl_verdict!(
                            hl_log::tag::WGPU,
                            "encoder_op.refused_in_pass",
                            op = ?std::mem::discriminant(other);
                            "encoder op inside a render pass cannot be executed there: {:?}",
                            std::mem::discriminant(other)
                        );
                        return Err(GpuError::Unsupported(
                            "wgpu: this encoder op cannot be executed inside a render pass",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
