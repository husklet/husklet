use super::*;

// (5) DEPTH — the previously-dropped depth attachment now drives a real depth test
// =================================================================================================

/// Draw two fullscreen triangles (via one pipeline, two vertex buffers) at different depths with `LESS` +
/// depth-write. `first` is drawn first, `second` second; returns the color plane. With no depth test, the
/// second draw always wins; with a real depth test the nearer fragment wins regardless of order.
fn depth_two_draws(near_first: bool) -> Vec<u8> {
    let vbuf = |z: f32, rgba: [f32; 4]| {
        let mut b = Vec::new();
        for (px, py) in [(-1.0f32, -1.0f32), (3.0, -1.0), (-1.0, 3.0)] {
            b.extend_from_slice(&px.to_le_bytes());
            b.extend_from_slice(&py.to_le_bytes());
            b.extend_from_slice(&z.to_le_bytes());
            for c in rgba {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        b
    };
    // green near (z=0.2), red far (z=0.8) — draw order depends on `near_first`.
    let (za, ca, zb, cb) = if near_first {
        (0.2f32, [0.0, 1.0, 0.0, 1.0], 0.8f32, [1.0, 0.0, 0.0, 1.0])
    } else {
        (0.8f32, [1.0, 0.0, 0.0, 1.0], 0.2f32, [0.0, 1.0, 0.0, 1.0])
    };
    let ba = vbuf(za, ca);
    let bb = vbuf(zb, cb);
    let spirv = wgsl_to_spirv(SEED_POS3_COLOR);
    let layout = VertexLayout {
        stride: 28,
        step_mode: 0,
        attrs: vec![
            VertexAttr {
                location: 0,
                format: vfmt(3, 0, false),
                offset: 0,
            },
            VertexAttr {
                location: 1,
                format: vfmt(4, 0, false),
                offset: 12,
            },
        ],
    };
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(
                2,
                tex(
                    4,
                    4,
                    TextureFormat::Depth32Float,
                    texture_usage::RENDER_TARGET,
                ),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateBuffer(
                1,
                buf(
                    ba.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::CreateBuffer(
                2,
                buf(
                    bb.len() as u64,
                    buffer_usage::VERTEX | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: ba,
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: bb,
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
                    depth: Some(DepthState::depth_only(
                        TextureFormat::Depth32Float,
                        true,
                        compare::LESS,
                    )),
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
                        depth: Some(DepthAttachment {
                            texture: 2,
                            depth_load: LoadOp::Clear,
                            stencil_load: LoadOp::Clear,
                            clear_depth: 1.0,
                            clear_stencil: 0,
                        }),
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
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 2,
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
    g.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn depth_test_occludes_farther_fragment() {
    // Near green drawn first, far red second: the far fragment fails LESS and is discarded → green stays.
    // Without the depth test the later red draw would overwrite → this asserts depth is really applied.
    all_texels_eq(&depth_two_draws(true), [0, 255, 0, 255]);
}

#[test]
fn depth_test_lets_nearer_fragment_through() {
    // Reverse order: far red drawn first (writes depth 0.8), near green drawn second. The nearer fragment
    // passes LESS (0.2 < 0.8) and overwrites → green. Together with the occlusion test this shows the depth
    // result is order-independent: the nearer fragment wins whether it is drawn first or last.
    all_texels_eq(&depth_two_draws(false), [0, 255, 0, 255]);
}

// =================================================================================================
