use super::*;

/// (10) DEPTH-TESTED draw: two fullscreen triangles at different constant depths through one pipeline
/// (`LESS` + depth-write). The nearer fragment wins on both backends regardless of draw order → the winning
/// flat colour is EXACT (±1). We alternate which of the two is nearer by seed parity.
pub(super) fn gen_draw_depth(seed: u64) -> Prog {
    let w = 4 + (seed % 4) as u32;
    let h = 4 + (seed % 3) as u32;
    let near_first = seed.is_multiple_of(2);
    let (za, ca, zb, cb) = if near_first {
        (
            0.25f32,
            fcolor_opaque(seed),
            0.75f32,
            fcolor_opaque(seed.wrapping_add(7)),
        )
    } else {
        (
            0.75f32,
            fcolor_opaque(seed.wrapping_add(7)),
            0.25f32,
            fcolor_opaque(seed),
        )
    };
    let vbuf = |z: f32, c: [f32; 4]| -> Vec<u8> {
        FS_TRI
            .iter()
            .flat_map(|(x, y)| le_f32(&[*x, *y, z, c[0], c[1], c[2], c[3]]))
            .collect::<Vec<u8>>()
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
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateTexture(2, depth_tex(w, h)),
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
                        load: LoadOp::Clear,
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
    ];
    Prog {
        seed,
        category: "draw_depth",
        ops: vec![
            "BeginRenderPass(depth)",
            "SetPipeline",
            "SetVertexBuffer",
            "Draw",
            "EndRenderPass",
        ],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 1,
        kernel: None,
    }
}

/// (11) BLENDED draw: an opaque fullscreen background (blend disabled → replace) then a translucent
/// fullscreen foreground whose pipeline blend is EXACTLY the equation the CPU oracle hardcodes —
/// colour = `(SrcAlpha, OneMinusSrcAlpha, Add)`, alpha = `(One, OneMinusSrcAlpha, Add)` — so
/// `out = fg*a + bg*(1-a)` on colour and `a + bg_a*(1-a)` on alpha match on both backends. ±2.
pub(super) fn gen_draw_blend(seed: u64) -> Prog {
    let w = 4 + (seed % 4) as u32;
    let h = 4 + (seed % 3) as u32;
    let bg = fcolor_opaque(seed);
    let a = [0.25f32, 0.5, 0.75][(seed % 3) as usize];
    let fg = [
        chan(seed, 5) as f32 / 255.0,
        chan(seed, 6) as f32 / 255.0,
        chan(seed, 7) as f32 / 255.0,
        a,
    ];
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = || VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr {
                location: 0,
                format: vfmt(2, 0, false),
                offset: 0,
            },
            VertexAttr {
                location: 1,
                format: vfmt(4, 0, false),
                offset: 8,
            },
        ],
    };
    let vbuf = |c: [f32; 4]| -> Vec<u8> {
        FS_TRI
            .iter()
            .flat_map(|(x, y)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]]))
            .collect::<Vec<u8>>()
    };
    // The CPU oracle's straight-alpha-over, expressed as a protocol BlendState (wire factors: 1=One,
    // 4=SrcAlpha, 5=OneMinusSrcAlpha; op 0=Add).
    let over = BlendState {
        src_color: 4,
        dst_color: 5,
        op_color: 0,
        src_alpha: 1,
        dst_alpha: 5,
        op_alpha: 0,
    };
    let opaque_target = ColorTargetState {
        format: TextureFormat::Rgba8Unorm,
        blend: None,
        write_mask: 0xF,
    };
    let blend_target = ColorTargetState {
        format: TextureFormat::Rgba8Unorm,
        blend: Some(over),
        write_mask: 0xF,
    };
    let pipe = |id: u32, target: ColorTargetState| {
        Cmd::CreateRenderPipeline(
            id,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "vs_main".into(),
                },
                fragment: Some(ShaderRef {
                    module: 1,
                    entry: "fs_main".into(),
                }),
                vertex_buffers: vec![layout()],
                color_targets: vec![target],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        )
    };
    let cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv,
        },
        Cmd::CreateBuffer(
            1,
            buf(24 * 3, buffer_usage::VERTEX | buffer_usage::COPY_DST),
        ),
        Cmd::CreateBuffer(
            2,
            buf(24 * 3, buffer_usage::VERTEX | buffer_usage::COPY_DST),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vbuf(bg),
        },
        Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: vbuf(fg),
        },
        pipe(1, opaque_target),
        pipe(2, blend_target),
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
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::SetPipeline(2),
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
    ];
    Prog {
        seed,
        category: "draw_blend",
        ops: vec![
            "BeginRenderPass",
            "SetPipeline",
            "SetVertexBuffer",
            "Draw(blend)",
            "EndRenderPass",
        ],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 2,
        kernel: None,
    }
}
