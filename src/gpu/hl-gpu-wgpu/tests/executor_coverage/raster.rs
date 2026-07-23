use super::*;

#[test]
fn instanced_per_instance_step_mode_advances_attribute() {
    // slot0 = per-vertex position (fullscreen triangle). slot1 = PER-INSTANCE Unorm8x4 color, stride 4.
    // Two instances draw the same triangle; the second (green) is last so it wins the color target.
    // If the step mode were wrongly per-vertex, the color attribute would index by vertex → red/green/blue
    // interpolation, NOT a uniform green — so an all-green readback proves per-instance stepping works.
    let mut posb = Vec::new();
    for (px, py) in [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)] {
        posb.extend_from_slice(&px.to_le_bytes());
        posb.extend_from_slice(&py.to_le_bytes());
    }
    // 4 colors so a (wrong) per-vertex read of vertex index up to 2 stays in-bounds: red, green, blue, white.
    let colorb: Vec<u8> = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let pos_layout = VertexLayout {
        stride: 8,
        step_mode: 0,
        attrs: vec![VertexAttr {
            location: 0,
            format: vfmt(2, 0, false),
            offset: 0,
        }],
    };
    let color_layout = VertexLayout {
        stride: 4,
        step_mode: 1, // per-instance
        attrs: vec![VertexAttr {
            location: 1,
            format: vfmt(4, 1, true),
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
                    posb.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::CreateBuffer(
                2,
                buf(
                    colorb.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: posb,
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: colorb,
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
                    vertex_buffers: vec![pos_layout, color_layout],
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
                    Enc::SetVertexBuffer {
                        slot: 1,
                        buffer: 2,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 2,
                        first_vertex: 0,
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

#[test]
fn scissor_restricts_draw_to_subrect() {
    // Clear the 4x4 target red, scissor to the inner (1,1)-(3,3) 2x2 box, draw a fullscreen green triangle.
    // Only the scissored texels turn green.
    let spirv = wgsl_to_spirv(SEED_VINDEX_GREEN);
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
                    vertex_buffers: vec![],
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
                    Enc::SetScissor {
                        x: 1,
                        y: 1,
                        w: 2,
                        h: 2,
                    },
                    Enc::SetPipeline(1),
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
    let px = g.read_texture(&s.resources, 1).unwrap();
    let green = [0u8, 255, 0, 255];
    let red = [255u8, 0, 0, 255];
    for y in 0..4u32 {
        for x in 0..4u32 {
            let t = &px[((y * 4 + x) * 4) as usize..][..4];
            let inside = (1..3).contains(&x) && (1..3).contains(&y);
            assert_eq!(t, if inside { green } else { red }, "pixel ({x},{y})");
        }
    }
}

// =================================================================================================
