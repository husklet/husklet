use super::*;

#[test]
fn vertex_buffer_two_attributes_float_and_unorm8() {
    // A fullscreen triangle: pos = Float32x2 (@loc0), color = Unorm8x4 (@loc1). Uniform blue at every
    // vertex, so every pixel reads back exactly [0,0,255,255] regardless of interpolation.
    let mut vbytes = Vec::new();
    for (px, py) in [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)] {
        vbytes.extend_from_slice(&px.to_le_bytes());
        vbytes.extend_from_slice(&py.to_le_bytes());
        vbytes.extend_from_slice(&[0, 0, 255, 255]); // Unorm8x4 RGBA
    }
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = VertexLayout {
        stride: 12,
        step_mode: 0,
        attrs: vec![
            VertexAttr {
                location: 0,
                format: vfmt(2, 0, false),
                offset: 0,
            },
            VertexAttr {
                location: 1,
                format: vfmt(4, 1, true),
                offset: 8,
            },
        ],
    };
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateBuffer(
                1,
                buf(
                    vbytes.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vbytes,
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 1,
                        entry: "fs_main".into(),
                    }),
                    vertex_buffers: vec![layout],
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
                            clear: [1.0, 0.0, 0.0, 1.0],
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
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 0, 255, 255]);
}

#[test]
fn indexed_quad_covers_target() {
    // Two triangles from 4 vertices + a U16 index buffer, covering the whole target with green.
    let mut vbytes = Vec::new();
    for (px, py) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        vbytes.extend_from_slice(&px.to_le_bytes());
        vbytes.extend_from_slice(&py.to_le_bytes());
    }
    let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
    let ibytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let spirv = wgsl_to_spirv(SEED_POS2_GREEN);
    let layout = VertexLayout {
        stride: 8,
        step_mode: 0,
        attrs: vec![VertexAttr {
            location: 0,
            format: vfmt(2, 0, false),
            offset: 0,
        }],
    };
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateBuffer(
                1,
                buf(
                    vbytes.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::CreateBuffer(
                2,
                buf(
                    ibytes.len() as u64,
                    buffer_usage::INDEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vbytes,
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: ibytes,
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 1,
                        entry: "fs_main".into(),
                    }),
                    vertex_buffers: vec![layout],
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
                            clear: [1.0, 0.0, 0.0, 1.0],
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
                    Enc::SetIndexBuffer {
                        buffer: 2,
                        offset: 0,
                        format: hl_gpu::protocol::model::enums::IndexFormat::U16,
                    },
                    Enc::DrawIndexed {
                        index_count: 6,
                        instance_count: 1,
                        first_index: 0,
                        base_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 255, 0, 255]);
}
