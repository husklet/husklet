use super::*;

const SAMPLER_BINDING_OFFSET: u32 = 16;

/// `vkCmdBindPipeline` — remember the bound hl-GPU pipeline id + kind for the next pass.
pub fn cmd_bind_pipeline(
    dev: &mut Device,
    cb: VkCommandBuffer,
    pipeline: VkPipeline,
) -> Result<()> {
    let (ir, kind) = {
        let p = dev
            .pipelines
            .get(&pipeline)
            .ok_or(GpuError::Invalid("vkCmdBindPipeline: unknown VkPipeline"))?;
        (p.ir_id, p.kind)
    };
    let rec = dev.require_recording(cb)?;
    rec.bound_pipeline = Some(ir);
    rec.bound_pipeline_kind = Some(kind);
    Ok(())
}

/// `vkCmdBindDescriptorSets` — resolve each set's `binding -> (buffer, offset, range)` table into a
/// [`Cmd::CreateBindGroup`] (applying that set's `pDynamicOffsets`) and record the `(set, bind-group)`
/// pair to replay into the next pass. Ported from `command.rs::vkCmdBindDescriptorSets`.
pub fn cmd_bind_descriptor_sets(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    cb: VkCommandBuffer,
    first_set: u32,
    sets: &[VkDescriptorSet],
    dynamic_offsets: &[u32],
) -> Result<()> {
    let mut dyn_cursor = 0usize; // global cursor across all bound sets
    for (i, &dset) in sets.iter().enumerate() {
        // Saturating add: a hostile `first_set` near `u32::MAX` with >1 set must not overflow the set
        // index (it is only a bind-group label here; a real firstSet is bounded by maxBoundDescriptorSets).
        let set_index = first_set.saturating_add(i as u32);
        // Snapshot the set's (binding -> buffer) table + its layout handle (owned; borrows end here).
        let Some(rec) = dev.descriptor_sets.get(&dset) else {
            continue;
        };
        let layout_handle = rec.layout;
        let mut pairs: Vec<(u32, (VkBuffer, u64, u64))> =
            rec.buffers.iter().map(|(b, v)| (*b, *v)).collect();
        pairs.sort_by_key(|(b, _)| *b);
        // Snapshot the set's sampled-image / sampler descriptors (binding-ascending; the borrow of
        // `rec` ends here so `dev` can be mutated below).
        let mut img_pairs: Vec<(u32, crate::model::descriptor::ImageBinding)> =
            rec.images.iter().map(|(b, v)| (*b, *v)).collect();
        img_pairs.sort_by_key(|(b, _)| *b);
        // Consume this set's dynamic offsets (its layout's dynamic-buffer bindings, ascending).
        let dyn_bindings = dev
            .set_layouts
            .get(&layout_handle)
            .map(|l| l.dynamic_bindings())
            .unwrap_or_default();
        let mut extra: HashMap<u32, u64> = HashMap::new();
        for db in dyn_bindings {
            if dyn_cursor < dynamic_offsets.len() {
                extra.insert(db, dynamic_offsets[dyn_cursor] as u64);
                dyn_cursor += 1;
            }
        }
        // Resolve each binding's buffer handle to its hl-GPU id, applying the dynamic offset.
        let mut entries: Vec<BindEntry> = Vec::new();
        for (binding, (buf_handle, offset, size)) in pairs {
            if let Some(b) = dev.buffers.get(&buf_handle) {
                entries.push(BindEntry {
                    binding,
                    resource: BindResource::Buffer {
                        id: b.ir_id,
                        offset: offset + extra.get(&binding).copied().unwrap_or(0),
                        size,
                    },
                });
            }
        }
        // Resolve each sampled-image / sampler descriptor to its hl-GPU id: a bound image → a
        // `BindResource::Texture` and a bound sampler → a `BindResource::Sampler`. A `COMBINED_IMAGE_SAMPLER`
        // (both set on one binding) must land on TWO DISTINCT bind-group bindings, because the wgpu executor
        // splits glslang's combined `sampler2D` into a separate `texture_2d` + `sampler` (naga rejects the
        // combined model): the image keeps binding `B`, the sampler moves to `B + SAMPLER_BINDING_OFFSET`,
        // matching `hl-gpu-wgpu::spirv_split`. A SEPARATE `SAMPLED_IMAGE`/`SAMPLER` keeps its own binding
        // (the shader already declares it separately). An unresolvable handle is skipped (never faked).
        for (binding, img) in img_pairs {
            let combined = img.image.is_some() && img.sampler.is_some();
            if let Some(image) = img.image {
                if let Some(i) = dev.images.get(&image) {
                    entries.push(BindEntry {
                        binding,
                        resource: BindResource::Texture { id: i.ir_id },
                    });
                }
            }
            if let Some(sampler) = img.sampler {
                if let Some(s) = dev.samplers.get(&sampler) {
                    let sampler_binding = if combined {
                        binding + SAMPLER_BINDING_OFFSET
                    } else {
                        binding
                    };
                    entries.push(BindEntry {
                        binding: sampler_binding,
                        resource: BindResource::Sampler { id: s.ir_id },
                    });
                }
            }
        }
        let ir_id = dev.alloc_ir();
        hl_log::hl_debug!(
            hl_log::tag::VULKAN,
            "bindgroup set={} ir={} entries={}",
            set_index,
            ir_id,
            entries.len()
        );
        sink.submit(&[Cmd::CreateBindGroup(
            ir_id,
            BindGroupDesc {
                set: set_index,
                entries,
            },
        )])?;
        if let Ok(cbrec) = dev.require_recording(cb) {
            cbrec.pending_bind_groups.push((set_index, ir_id));
        }
    }
    Ok(())
}

/// `vkCmdDispatch` — record a compute pass: `BeginComputePass` → `SetPipeline` → `SetBindGroup`* →
/// `Dispatch` → `EndComputePass`. Ported from `command.rs::vkCmdDispatch`.
pub fn cmd_dispatch(dev: &mut Device, cb: VkCommandBuffer, x: u32, y: u32, z: u32) -> Result<()> {
    let rec = dev.require_recording(cb)?;
    let pipeline = rec.bound_pipeline;
    let groups = rec.pending_bind_groups.clone();
    rec.enc.push(Enc::BeginComputePass);
    if let Some(p) = pipeline {
        rec.enc.push(Enc::SetPipeline(p));
    }
    for (index, group) in groups {
        rec.enc.push(Enc::SetBindGroup { index, group });
    }
    rec.enc.push(Enc::Dispatch { x, y, z });
    rec.enc.push(Enc::EndComputePass);
    Ok(())
}
