use super::*;
use crate::model::program::Program;

pub(super) struct CaptureInputs<'a> {
    pub(super) layouts: &'a [VertexLayout],
    pub(super) slot_ir: &'a [u32],
    pub(super) slot_base: &'a [u32],
    pub(super) client_slots: &'a [ClientSlot],
    pub(super) index_ir: u32,
    pub(super) expanded_indices: bool,
    pub(super) app_bind_entries: &'a [BindEntry],
    pub(super) color_targets: &'a [ColorTargetState],
    pub(super) depth_format: Option<TextureFormat>,
    pub(super) sample_count: u32,
}

/// Build the correctness-first transform-feedback pass for one GL draw. Each logical vertex invocation is
/// a one-point draw retaining its original vertex/index/instance identifiers. A distinct aligned offset
/// uniform directs it to disjoint output words, so capture order never depends on GPU scheduling.
pub(super) fn lower_transform_feedback(
    ctx: &mut GlContext,
    draw: &DrawCall,
    program: &Program,
    inputs: CaptureInputs<'_>,
    cmds: &mut Vec<Cmd>,
) -> Option<Vec<Enc>> {
    let capture = draw.transform_feedback.as_ref()?;
    if inputs.expanded_indices {
        // Loop/fan index expansion changes the invocation stream. It needs an explicit original-index
        // mapping before it can be captured without duplicating strip/fan vertices.
        return None;
    }
    if capture.vertices == 0 {
        return Some(Vec::new());
    }
    let shader_words = program.transform_feedback_ir.clone()?;
    let shader = ctx.alloc_shader_ir().ok()?;
    let fragment_shader = ctx.alloc_shader_ir().ok()?;
    let pipeline = ctx.alloc_pipeline_ir().ok()?;
    cmds.push(Cmd::CreateShader {
        id: shader,
        kind: ShaderPayloadKind::Glsl,
        spirv: shader_words,
    });
    cmds.push(Cmd::CreateShader {
        id: fragment_shader,
        kind: ShaderPayloadKind::Glsl,
        spirv: hl_gpu::protocol::model::kernel::GlslDescriptor {
            stage: hl_gpu::protocol::model::kernel::glsl_stage::FRAGMENT,
            entry: "fmain".into(),
            // A private fragment shader is required because WebGPU render pipelines cannot be targetless.
            // It has no application-visible effects; color writes are disabled on every target below.
            source:
                "#version 460\nlayout(location=0) out vec4 hl_tf_color; void main(){ discard; }"
                    .into(),
        }
        .to_words(),
    });
    let mut neutral = draw.clone();
    neutral.depth = false;
    neutral.depth_write = false;
    neutral.stencil = false;
    let depth = inputs
        .depth_format
        .map(|format| Pipeline::depth_state(format, &neutral));
    cmds.push(Cmd::CreateRenderPipeline(
        pipeline,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: shader,
                entry: "vmain".into(),
            },
            fragment: Some(ShaderRef {
                module: fragment_shader,
                entry: "fmain".into(),
            }),
            vertex_buffers: inputs.layouts.to_vec(),
            color_targets: inputs
                .color_targets
                .iter()
                .cloned()
                .map(|mut target| {
                    target.blend = None;
                    target.write_mask = 0;
                    target
                })
                .collect(),
            depth,
            topology: Topology::PointList,
            cull: 0,
            front_face: 0,
            sample_count: inputs.sample_count,
            label: "gl-transform-feedback".into(),
        },
    ));

    let mut outputs = Vec::with_capacity(capture.layout.buffers as usize);
    for index in 0..capture.layout.buffers as usize {
        let len = usize::try_from(capture.byte_lengths[index]).ok()?;
        if len == 0 {
            continue;
        }
        let ir = ctx.alloc_buffer_ir().ok()?;
        cmds.push(Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size: len as u64,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC,
                label: "gl-transform-feedback-output".into(),
            },
        ));
        let binding = capture.bindings[index]?;
        ctx.local.transform_feedback_readbacks.push(
            crate::model::context::TransformFeedbackReadback {
                ir,
                buffer: binding.buffer,
                offset: usize::try_from(capture.byte_offsets[index]).ok()?,
                len,
            },
        );
        ctx.local
            .transform_feedback_cleanup
            .push(Cmd::DestroyBuffer(ir));
        outputs.push((index as u32, ir, len));
    }

    let invocation_count = capture.vertices as usize;
    let offset_size = invocation_count.checked_mul(256)?;
    let offsets = ctx.alloc_buffer_ir().ok()?;
    let mut offset_bytes = vec![0u8; offset_size];
    for invocation in 0..invocation_count {
        for buffer in 0..capture.layout.buffers as usize {
            let words = invocation.checked_mul(capture.layout.strides[buffer] as usize / 4)?;
            let value = u32::try_from(words).ok()?.to_le_bytes();
            let at = invocation * 256 + buffer * 4;
            offset_bytes[at..at + 4].copy_from_slice(&value);
        }
    }
    cmds.push(Cmd::CreateBuffer(
        offsets,
        BufferDesc {
            size: offset_size as u64,
            usage: buffer_usage::UNIFORM,
            label: "gl-transform-feedback-offsets".into(),
        },
    ));
    cmds.push(Cmd::WriteBuffer {
        id: offsets,
        offset: 0,
        data: offset_bytes,
    });
    ctx.local.transform_feedback_cleanup.extend([
        Cmd::DestroyBuffer(offsets),
        Cmd::DestroyShader(shader),
        Cmd::DestroyShader(fragment_shader),
        Cmd::DestroyPipeline(pipeline),
    ]);

    let mut groups = Vec::with_capacity(invocation_count);
    for invocation in 0..invocation_count {
        let group = ctx.alloc_bind_group_ir().ok()?;
        let mut entries = inputs.app_bind_entries.to_vec();
        entries.extend(outputs.iter().map(|&(binding, id, len)| BindEntry {
            binding: 64 + binding,
            resource: BindResource::Buffer {
                id,
                offset: 0,
                size: len as u64,
            },
        }));
        entries.push(BindEntry {
            binding: 68,
            resource: BindResource::Buffer {
                id: offsets,
                offset: (invocation * 256) as u64,
                size: 16,
            },
        });
        cmds.push(Cmd::CreateBindGroup(
            group,
            BindGroupDesc { set: 0, entries },
        ));
        ctx.local
            .transform_feedback_cleanup
            .push(Cmd::DestroyBindGroup(group));
        groups.push(group);
    }

    let mut ops = vec![Enc::SetPipeline(pipeline)];
    for (slot, (&buffer, &offset)) in inputs.slot_ir.iter().zip(inputs.slot_base).enumerate() {
        ops.push(Enc::SetVertexBuffer {
            slot: slot as u32,
            buffer,
            offset: offset as u64,
        });
    }
    for (index, slot) in inputs.client_slots.iter().enumerate() {
        ops.push(Enc::SetVertexBuffer {
            slot: (inputs.slot_ir.len() + index) as u32,
            buffer: slot.ir,
            offset: 0,
        });
    }
    if draw.indexed {
        let format = if draw.index_type == GL_UNSIGNED_INT {
            hl_gpu::protocol::model::enums::IndexFormat::U32
        } else {
            hl_gpu::protocol::model::enums::IndexFormat::U16
        };
        ops.push(Enc::SetIndexBuffer {
            buffer: inputs.index_ir,
            offset: if draw.elem_buf == 0 {
                0
            } else {
                draw.index_offset as u64
            },
            format,
        });
    }
    let vertices_per_instance = capture.vertices / draw.instance_count.max(1);
    let mut invocation = 0usize;
    for instance in 0..draw.instance_count {
        for vertex in 0..vertices_per_instance {
            ops.push(Enc::SetBindGroup {
                index: 0,
                group: groups[invocation],
            });
            if draw.indexed {
                ops.push(Enc::DrawIndexed {
                    index_count: 1,
                    instance_count: 1,
                    first_index: vertex,
                    base_vertex: draw.base_vertex,
                    first_instance: draw.first_instance + instance,
                });
            } else {
                ops.push(Enc::Draw {
                    vertex_count: 1,
                    instance_count: 1,
                    first_vertex: draw.first.max(0) as u32 + vertex,
                    first_instance: draw.first_instance + instance,
                });
            }
            invocation += 1;
        }
    }
    Some(ops)
}
