use super::*;

// =================================================================================================
// (6) BAD indices + count overflows
// =================================================================================================

#[test]
fn bind_group_index_out_of_range_is_invalid() {
    let mut g = exec();
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::CreateBuffer(1, buf(16, buffer_usage::UNIFORM)));
    cmds.push(Cmd::CreateBindGroup(
        1,
        BindGroupDesc {
            set: 0,
            entries: vec![BindEntry {
                binding: 0,
                resource: BindResource::Buffer {
                    id: 1,
                    offset: 0,
                    size: 16,
                },
            }],
        },
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 7, group: 1 }, // >= max_bind_groups (4)
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "bind_index_oor", &cmds, is_invalid);
}

#[test]
fn vertex_buffer_offset_beyond_buffer_is_oob() {
    let mut g = exec();
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::CreateBuffer(
        1,
        buf(32, buffer_usage::VERTEX | buffer_usage::COPY_DST),
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 1,
                offset: 4096,
            }, // past the 32-byte buffer -> would panic slice()
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "vbuf_bad_offset", &cmds, is_oob);
}

#[test]
fn draw_range_overflow_is_invalid() {
    let mut g = exec();
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(1),
            // first_vertex + vertex_count overflows u32 -> would panic building the draw range.
            Enc::Draw {
                vertex_count: 100,
                instance_count: 1,
                first_vertex: u32::MAX - 10,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "draw_overflow", &cmds, is_invalid);
}

#[test]
fn draw_vertex_count_beyond_bound_buffer_is_invalid() {
    let mut g = exec();
    // A pipeline that reads a per-vertex attribute, a tiny (24-byte) vertex buffer, and a draw of 100000
    // vertices — wgpu rejects the overrun at pass-end; the validation-scope net makes it a typed error.
    let vs = "#version 460\nlayout(location=0) in vec2 p; void main(){ gl_Position = vec4(p,0.0,1.0); }\n";
    let fs = "#version 460\nlayout(location=0) out vec4 c; void main(){ c = vec4(1.0); }\n";
    hostile(
        &mut g,
        "draw_beyond_vbuf",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(1, buf(24, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 8,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location: 0,
                            format: 2,
                            offset: 0,
                        }],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 1,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 100_000,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
        is_invalid,
    );
}
