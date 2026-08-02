use super::*;

impl WgpuExecutor {
    /// Execute one compute pass: for each dispatch, build a concrete bind group for every set index the
    /// stream currently has bound, each against that group's layout on the bound pipeline
    /// (`get_bind_group_layout(index)`), then replay, submit, and wait.
    ///
    /// Unlike the earlier single-group path this honors `SetBindGroup.index`, so a SPIR-V compute pipeline
    /// with 2+ bind groups (e.g. wgpu-core's own indirect-draw-validation pipeline: group 0 the params,
    /// group 1 the indirect buffers) binds each group at its declared set. Dynamic offsets are already
    /// baked into each `BindResource::Buffer.offset` by the shim (the protocol's `SetBindGroup` carries no
    /// separate offset list, and an auto layout reflects no dynamic-offset binding), so the wgpu offsets
    /// slice is empty. Push constants are likewise not a protocol op — a compute pipeline that declares a
    /// `var<push_constant>` needs it only to CREATE (that is the wgpu-core validation pipeline Zed builds
    /// during device creation, which is never dispatched through this executor); reading push constants at
    /// a dispatch that the protocol never wrote is not reachable here.
    pub(super) fn run_compute_pass(
        &mut self,
        res: &SessionResources,
        ops: &[Enc],
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        // As `run_render_pass`: wrap the pass in a validation scope so an over-large dispatch or other wgpu
        // rejection surfaces as a typed error instead of the panicking default handler.
        self.with_validation_scope(|s| s.run_compute_pass_inner(res, ops, encoder))
    }

    pub(super) fn run_compute_pass_inner(
        &mut self,
        res: &SessionResources,
        ops: &[Enc],
        enc: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::EXEC, "submit");
        hl_log::hl_count!(tag::EXEC, "passes");
        // Current bind-group binding per set index. Sized to the advertised `max_bind_groups` (4); a higher
        // index would have been rejected by the runtime before reaching here.
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_groups: [Option<u32>; 4] = [None; 4];
        // (specialized pipeline, [(set index, bind group)], (x,y,z)) per Dispatch.
        #[allow(clippy::type_complexity)]
        let mut dispatches: Vec<(
            wgpu::ComputePipeline,
            Vec<(u32, wgpu::BindGroup)>,
            (u32, u32, u32),
        )> = Vec::new();
        for op in ops {
            match op {
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { index, group } => {
                    let slot = *index as usize;
                    if slot >= cur_groups.len() {
                        return Err(GpuError::Invalid(
                            "wgpu: compute bind-group index out of range",
                        ));
                    }
                    cur_groups[slot] = Some(*group);
                }
                Enc::Dispatch { x, y, z } => {
                    hl_log::hl_count!(tag::EXEC, "dispatches");
                    let pid = cur_pipeline.ok_or(GpuError::Invalid("dispatch with no pipeline"))?;
                    if let PipelineNative::Render { .. } = PipelineNative::get(res, pid)? {
                        return Err(GpuError::Unsupported("wgpu: dispatch on a render pipeline"));
                    }
                    let descriptors = cur_groups
                        .iter()
                        .filter_map(|group| group.map(|id| self.bind_group(res, id)))
                        .collect::<Result<Vec<_>>>()?;
                    let alignment = self.gpu.device.limits().min_storage_buffer_offset_alignment as u64;
                    let specialization = crate::texel_buffer::key(descriptors.iter().copied(), alignment)?;
                    let pipeline = self.compute_pipeline_for(res, pid, &specialization)?;
                    let remap_group_zero = match PipelineNative::get(res, pid)? {
                        PipelineNative::Compute {
                            remap_group_zero, ..
                        } => *remap_group_zero,
                        PipelineNative::Render { .. } => unreachable!("checked above"),
                    };
                    let mut groups = Vec::new();
                    for (idx, bound) in cur_groups.iter().enumerate() {
                        if let Some(g) = bound {
                            let layout = pipeline.get_bind_group_layout(idx as u32);
                            // Compute bind groups already match their layout (the kernel path's explicit
                            // layout / the SPIR-V path's auto layout are built to), so no filter is applied.
                            let descriptor = self.bind_group(res, *g)?;
                            let views = crate::texel_buffer::views(res, descriptor, alignment)?;
                            let bg = self.build_bind_group(
                                res,
                                &layout,
                                descriptor,
                                None,
                                remap_group_zero,
                                Some(&views),
                                None,
                            )?;
                            groups.push((idx as u32, bg));
                        }
                    }
                    if groups.is_empty() {
                        return Err(GpuError::Invalid("dispatch with no bind group"));
                    }
                    // A workgroup count that exceeds the device's per-dimension ceiling is a hard wgpu
                    // validation error at submit (its handler panics). Reject an over-large dispatch here as
                    // a typed error; the runtime does not range-check `Dispatch`. (A zero count is legal — a
                    // no-op dispatch, exercised by `compute_zero_dispatch_is_noop_not_panic`.)
                    let max = self
                        .gpu
                        .device
                        .limits()
                        .max_compute_workgroups_per_dimension;
                    if *x > max || *y > max || *z > max {
                        return Err(GpuError::OutOfBounds);
                    }
                    dispatches.push((pipeline, groups, (*x, *y, *z)));
                }
                _ => {}
            }
        }

        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hl-compute-pass"),
                timestamp_writes: None,
            });
            for (pipeline, groups, (x, y, z)) in &dispatches {
                pass.set_pipeline(pipeline);
                for (idx, bg) in groups {
                    pass.set_bind_group(*idx, bg, &[]);
                }
                pass.dispatch_workgroups(*x, *y, *z);
            }
        }
        Ok(())
    }
}
