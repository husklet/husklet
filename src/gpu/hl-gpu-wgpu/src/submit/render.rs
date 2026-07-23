use super::*;

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
    ) -> Result<()> {
        // Run the pass under a validation scope so a wgpu rejection at pass-end/submit (e.g. a draw whose
        // vertex/index count overruns the bound buffer, or a depth-tested pipeline drawn with no depth
        // attachment) is a typed error, not the panicking default handler — and the executor survives.
        self.with_validation_scope(|s| s.run_render_pass_inner(res, color, depth, ops))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_render_pass_inner(
        &mut self,
        res: &SessionResources,
        color: &[ColorAttachment],
        depth: Option<&DepthAttachment>,
        ops: &[Enc],
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::EXEC, "submit");
        hl_log::hl_count!(tag::EXEC, "passes");
        // Resolve attachment views up front (they must outlive the pass). While resolving, track the render
        // pass's render extent (`target_w`/`target_h`) — the MIN width/height across all attachments, which
        // is exactly the bound wgpu validates a viewport against (`state.info.extent`). It is the clip
        // rectangle a GL-style out-of-bounds viewport must be intersected with (see `clamp_viewport`).
        let mut views = Vec::with_capacity(color.len());
        let mut target_w = u32::MAX;
        let mut target_h = u32::MAX;
        for c in color {
            let t = texture::WgpuTexture::get(res, c.texture)?;
            target_w = target_w.min(t.width);
            target_h = target_h.min(t.height);
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
        // Pre-build the bind groups every draw in this pass needs, keyed to the pipeline it's drawn with.
        // Bind-group binding is tracked PER SET INDEX (mirroring `run_compute_pass`): a draw builds a
        // concrete bind group for EVERY set index currently bound, each against THAT set's own layout on the
        // bound pipeline (`get_bind_group_layout(index)`) and bound at that index below. Sized to the
        // advertised `max_bind_groups` (4); a higher index would have been rejected by the runtime.
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_groups: [Option<u32>; 4] = [None; 4];
        // (pipeline id, [(set index, bind group)]) per Draw, in order. An empty group list is the bindingless
        // fast path (e.g. the conformance triangle) — unlike compute, a draw with no bind group is legal.
        #[allow(clippy::type_complexity)]
        let mut draws: Vec<(u32, Vec<(u32, wgpu::BindGroup)>)> = Vec::new();
        for op in ops {
            match op {
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { index, group } => {
                    let slot = *index as usize;
                    if slot >= cur_groups.len() {
                        return Err(GpuError::Invalid(
                            "wgpu: render bind-group index out of range",
                        ));
                    }
                    cur_groups[slot] = Some(*group);
                }
                Enc::Draw { .. } | Enc::DrawIndexed { .. } => {
                    hl_log::hl_count!(tag::EXEC, "draws");
                    let pid =
                        cur_pipeline.ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
                    let mut groups = Vec::new();
                    for (idx, bound) in cur_groups.iter().enumerate() {
                        if let Some(g) = bound {
                            let (layout, filter) = match PipelineNative::get(res, pid)? {
                                PipelineNative::Render {
                                    pipeline,
                                    used_bindings,
                                    ..
                                } => (
                                    pipeline.get_bind_group_layout(idx as u32),
                                    used_bindings.as_slice(),
                                ),
                                PipelineNative::Compute { .. } => {
                                    return Err(GpuError::Unsupported(
                                        "wgpu: draw on a compute pipeline",
                                    ))
                                }
                            };
                            // Filter the driver's bind-group entries to the bindings this pipeline's shaders
                            // actually read in THIS group (the filter keys on the bind group's own `set`, so it
                            // restricts to this set's slots), so a GskGpu bind group that carries an unsampled
                            // texture/sampler pair still matches (see `bindgroup::build_bind_group`).
                            let bg = self.build_bind_group(
                                res,
                                &layout,
                                self.bind_group(res, *g)?,
                                Some(filter),
                            )?;
                            groups.push((idx as u32, bg));
                        }
                    }
                    draws.push((pid, groups));
                }
                _ => {}
            }
        }

        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
                                    r: c.clear[0] as f64,
                                    g: c.clear[1] as f64,
                                    b: c.clear[2] as f64,
                                    a: c.clear[3] as f64,
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
                        match clamp_viewport(*x, *y, *w, *h, target_w, target_h) {
                            Some((cx, cy, cw, ch)) => {
                                vp_degenerate = false;
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
                    Enc::SetScissor { x, y, w, h } => pass.set_scissor_rect(*x, *y, *w, *h),
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
                        let (pid, groups) = &draws[di];
                        di += 1;
                        if let PipelineNative::Render { pipeline, .. } =
                            PipelineNative::get(res, *pid)?
                        {
                            pass.set_pipeline(pipeline);
                        }
                        for (idx, bg) in groups {
                            pass.set_bind_group(*idx, bg, &[]);
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
                        // A draw under a wholly-out-of-bounds viewport rasterizes nothing in GL; skip it
                        // (di/pipeline/binds already advanced) rather than draw through the default viewport.
                        if !vp_degenerate {
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
                        let (pid, groups) = &draws[di];
                        di += 1;
                        if let PipelineNative::Render { pipeline, .. } =
                            PipelineNative::get(res, *pid)?
                        {
                            pass.set_pipeline(pipeline);
                        }
                        for (idx, bg) in groups {
                            pass.set_bind_group(*idx, bg, &[]);
                        }
                        let idx_end = first_index
                            .checked_add(*index_count)
                            .ok_or(GpuError::Invalid("wgpu: draw index range overflow"))?;
                        let i_end = first_instance
                            .checked_add(*instance_count)
                            .ok_or(GpuError::Invalid("wgpu: draw instance range overflow"))?;
                        // See the `Draw` arm: a wholly-out-of-bounds viewport draws nothing in GL.
                        if !vp_degenerate {
                            pass.draw_indexed(
                                *first_index..idx_end,
                                *base_vertex,
                                *first_instance..i_end,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        self.gpu.queue.submit(Some(enc.finish()));
        self.gpu.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }
}
