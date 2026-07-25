use super::*;

/// (8) FLAT opaque draw: a fullscreen triangle, all three vertices the same opaque colour, blend disabled
/// (replace). Full coverage + identical unorm rounding of a constant → EXACT (±1 guard).
pub(super) fn gen_draw_flat(seed: u64) -> Prog {
    let c = fcolor_opaque(seed);
    let vbytes: Vec<u8> = FS_TRI
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]]))
        .collect();
    let w = 4 + (seed % 5) as u32; // 4..=8
    let h = 4 + (seed % 4) as u32;
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = VertexLayout {
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
    let cmds = draw_cmds(w, h, spirv, layout, vbytes, None, None);
    Prog {
        seed,
        category: "draw_flat",
        ops: vec![
            "BeginRenderPass",
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

/// (9) GRADIENT draw: a fullscreen triangle with three DISTINCT vertex colours. Both backends
/// barycentric-interpolate in f32 then quantise; ±2 for last-ULP interpolation + rounding-rule differences.
pub(super) fn gen_draw_gradient(seed: u64) -> Prog {
    let ca = fcolor_opaque(seed);
    let cb = fcolor_opaque(seed.wrapping_add(5));
    let cc = fcolor_opaque(seed.wrapping_add(11));
    let cols = [ca, cb, cc];
    let vbytes: Vec<u8> = FS_TRI
        .iter()
        .zip(cols)
        .flat_map(|((x, y), c)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]]))
        .collect();
    let w = 4 + (seed % 5) as u32;
    let h = 4 + (seed % 4) as u32;
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let layout = VertexLayout {
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
    let cmds = draw_cmds(w, h, spirv, layout, vbytes, None, None);
    Prog {
        seed,
        category: "draw_gradient",
        ops: vec![
            "BeginRenderPass",
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
        tol: 2,
        kernel: None,
    }
}

/// Build a single-draw render program: clear to `[0,0,0,1]`, bind one pipeline + vertex buffer (id 1), draw
/// the 3-vertex fullscreen triangle. `blend`/`depth` opt into a blended target / a depth attachment (id 2).
fn draw_cmds(
    w: u32,
    h: u32,
    spirv: Vec<u32>,
    layout: VertexLayout,
    vbytes: Vec<u8>,
    blend: Option<BlendState>,
    depth: Option<DepthState>,
) -> Vec<Cmd> {
    let depth_att = depth.as_ref().map(|_| DepthAttachment {
        texture: 2,
        load: LoadOp::Clear,
        clear_depth: 1.0,
        clear_stencil: 0,
    });
    let mut cmds = vec![
        Cmd::CreateTexture(1, tex(w, h)),
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
    ];
    if depth.is_some() {
        cmds.push(Cmd::CreateTexture(2, depth_tex(w, h)));
    }
    cmds.push(Cmd::CreateRenderPipeline(
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
                blend,
                write_mask: 0xF,
            }],
            depth,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
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
                depth: depth_att,
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
    }));
    cmds
}
