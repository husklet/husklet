use super::*;
use hl_gpu::protocol::model::descriptor::{PipelineBinding, PipelineBindingKind, PipelineLayout};

const SAMPLER_BINDING_OFFSET: u32 = 16;
type BufferDescriptor = ((u32, u32), (VkBuffer, u64, u64));

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
        let mut pairs: Vec<BufferDescriptor> = rec.buffers.iter().map(|(b, v)| (*b, *v)).collect();
        pairs.sort_by_key(|(b, _)| *b);
        // Snapshot the set's sampled-image / sampler descriptors (binding-ascending; the borrow of
        // `rec` ends here so `dev` can be mutated below).
        let mut img_pairs: Vec<((u32, u32), crate::model::descriptor::ImageBinding)> =
            rec.images.iter().map(|(b, v)| (*b, *v)).collect();
        img_pairs.sort_by_key(|(b, _)| *b);
        let mut texel_pairs: Vec<((u32, u32), VkBufferView)> =
            rec.texel_buffers.iter().map(|(b, v)| (*b, *v)).collect();
        texel_pairs.sort_by_key(|(b, _)| *b);
        let expected_counts = dev
            .set_layouts
            .get(&layout_handle)
            .map(|layout| {
                layout
                    .bindings
                    .iter()
                    .map(|binding| (binding.binding, binding.descriptor_count))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let descriptor_types = dev
            .set_layouts
            .get(&layout_handle)
            .map(|layout| {
                layout
                    .bindings
                    .iter()
                    .map(|binding| (binding.binding, binding.descriptor_type))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let scalar_layout = PipelineLayout {
            bindings: expected_counts
                .iter()
                .map(|(&binding, &count)| PipelineBinding {
                    group: set_index,
                    binding,
                    count,
                    // Scalar slot allocation depends only on group, binding and count.
                    kind: PipelineBindingKind::UniformBuffer,
                })
                .collect(),
        };
        // Consume this set's dynamic offsets (its layout's dynamic-buffer bindings, ascending).
        let dyn_bindings = dev
            .set_layouts
            .get(&layout_handle)
            .map(|l| l.dynamic_elements())
            .unwrap_or_default();
        let mut extra: HashMap<(u32, u32), u64> = HashMap::new();
        for element in dyn_bindings {
            if dyn_cursor < dynamic_offsets.len() {
                extra.insert(element, dynamic_offsets[dyn_cursor] as u64);
                dyn_cursor += 1;
            }
        }
        // Resolve each binding's buffer handle to its hl-GPU id, applying the dynamic offset.
        let mut entries: Vec<BindEntry> = Vec::new();
        let mut buffers = std::collections::BTreeMap::<u32, Vec<(u32, BufferBinding)>>::new();
        for ((binding, element), (buf_handle, offset, size)) in pairs {
            if let Some(buffer) = dev.buffers.get(&buf_handle) {
                buffers.entry(binding).or_default().push((
                    element,
                    BufferBinding {
                        id: buffer.ir_id,
                        offset: offset + extra.get(&(binding, element)).copied().unwrap_or(0),
                        size,
                    },
                ));
            }
        }
        for (binding, mut elements) in buffers {
            elements.sort_by_key(|(element, _)| *element);
            if elements
                .iter()
                .enumerate()
                .any(|(expected, (actual, _))| *actual != expected as u32)
            {
                return Err(GpuError::Invalid(
                    "descriptor buffer array has an unbound element",
                ));
            }
            if expected_counts.get(&binding).copied() != Some(elements.len() as u32) {
                return Err(GpuError::Invalid(
                    "descriptor buffer array is not fully bound",
                ));
            }
            let values = elements
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>();
            if values.len() == 1 {
                let value = values[0];
                entries.push(BindEntry {
                    binding,
                    resource: BindResource::Buffer {
                        id: value.id,
                        offset: value.offset,
                        size: value.size,
                    },
                });
            } else if descriptor_types.get(&binding).is_some_and(|descriptor| {
                matches!(
                    *descriptor,
                    crate::model::descriptor::vk_descriptor_type::UNIFORM_BUFFER
                        | crate::model::descriptor::vk_descriptor_type::UNIFORM_BUFFER_DYNAMIC
                        | crate::model::descriptor::vk_descriptor_type::STORAGE_BUFFER
                        | crate::model::descriptor::vk_descriptor_type::STORAGE_BUFFER_DYNAMIC
                )
            }) {
                for (element, value) in values.into_iter().enumerate() {
                    entries.push(BindEntry {
                        binding: scalar_layout.scalar_binding(
                            set_index,
                            binding,
                            element as u32,
                        )?,
                        resource: BindResource::Buffer {
                            id: value.id,
                            offset: value.offset,
                            size: value.size,
                        },
                    });
                }
            } else {
                entries.push(BindEntry {
                    binding,
                    resource: BindResource::BufferArray { elements: values },
                });
            }
        }
        for ((binding, element), view_handle) in texel_pairs {
            let view = dev.buffer_views.get(&view_handle).ok_or(GpuError::Invalid(
                "descriptor references unknown VkBufferView",
            ))?;
            let buffer = dev.buffers.get(&view.buffer).ok_or(GpuError::Invalid(
                "VkBufferView references unknown VkBuffer",
            ))?;
            let expected = expected_counts.get(&binding).copied().unwrap_or(0);
            if element >= expected {
                return Err(GpuError::OutOfBounds);
            }
            let native_binding = if expected > 1 {
                scalar_layout.scalar_binding(set_index, binding, element)?
            } else {
                binding
            };
            entries.push(BindEntry {
                binding: native_binding,
                resource: BindResource::TexelBuffer {
                    id: buffer.ir_id,
                    offset: view.offset,
                    size: view.range,
                    format: view.format,
                    writable: descriptor_types.get(&binding)
                        == Some(
                            &crate::model::descriptor::vk_descriptor_type::STORAGE_TEXEL_BUFFER,
                        ),
                },
            });
        }
        // Resolve each sampled-image / sampler descriptor to its hl-GPU id: a bound image → a
        // `BindResource::Texture` and a bound sampler → a `BindResource::Sampler`. A `COMBINED_IMAGE_SAMPLER`
        // (both set on one binding) must land on TWO DISTINCT bind-group bindings, because the wgpu executor
        // splits glslang's combined `sampler2D` into a separate `texture_2d` + `sampler` (naga rejects the
        // combined model): the image keeps binding `B`, the sampler moves to `B + SAMPLER_BINDING_OFFSET`,
        // matching `hl-gpu-wgpu::spirv_split`. A SEPARATE `SAMPLED_IMAGE`/`SAMPLER` keeps its own binding
        // (the shader already declares it separately). An unresolvable handle is skipped (never faked).
        let mut textures = std::collections::BTreeMap::<u32, Vec<(u32, u32)>>::new();
        let mut samplers = std::collections::BTreeMap::<u32, Vec<(u32, u32, bool)>>::new();
        for ((binding, element), img) in img_pairs {
            let combined = img.image.is_some() && img.sampler.is_some();
            if let Some(image) = img.image {
                if let Some(i) = dev.images.get(&image) {
                    textures
                        .entry(binding)
                        .or_default()
                        .push((element, i.ir_id));
                }
            }
            if let Some(sampler) = img.sampler {
                if let Some(s) = dev.samplers.get(&sampler) {
                    samplers
                        .entry(binding)
                        .or_default()
                        .push((element, s.ir_id, combined));
                }
            }
        }
        for (binding, mut elements) in textures {
            elements.sort_by_key(|(element, _)| *element);
            if elements
                .iter()
                .enumerate()
                .any(|(expected, (actual, _))| *actual != expected as u32)
            {
                return Err(GpuError::Invalid(
                    "descriptor texture array has an unbound element",
                ));
            }
            if expected_counts.get(&binding).copied() != Some(elements.len() as u32) {
                return Err(GpuError::Invalid(
                    "descriptor texture array is not fully bound",
                ));
            }
            let ids = elements.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
            if ids.len() > 1
                && descriptor_types.get(&binding)
                    == Some(&crate::model::descriptor::vk_descriptor_type::STORAGE_IMAGE)
            {
                for (element, id) in ids.into_iter().enumerate() {
                    entries.push(BindEntry {
                        binding: scalar_layout.scalar_binding(
                            set_index,
                            binding,
                            element as u32,
                        )?,
                        resource: BindResource::Texture { id },
                    });
                }
            } else {
                entries.push(BindEntry {
                    binding,
                    resource: if ids.len() == 1 {
                        BindResource::Texture { id: ids[0] }
                    } else {
                        BindResource::TextureArray { ids }
                    },
                });
            }
        }
        for (binding, mut elements) in samplers {
            elements.sort_by_key(|(element, _, _)| *element);
            if elements
                .iter()
                .enumerate()
                .any(|(expected, (actual, _, _))| *actual != expected as u32)
            {
                return Err(GpuError::Invalid(
                    "descriptor sampler array has an unbound element",
                ));
            }
            if expected_counts.get(&binding).copied() != Some(elements.len() as u32) {
                return Err(GpuError::Invalid(
                    "descriptor sampler array is not fully bound",
                ));
            }
            let combined = elements.iter().all(|(_, _, combined)| *combined);
            let ids = elements
                .into_iter()
                .map(|(_, id, _)| id)
                .collect::<Vec<_>>();
            entries.push(BindEntry {
                binding: if combined {
                    binding + SAMPLER_BINDING_OFFSET
                } else {
                    binding
                },
                resource: if ids.len() == 1 {
                    BindResource::Sampler { id: ids[0] }
                } else {
                    BindResource::SamplerArray { ids }
                },
            });
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
