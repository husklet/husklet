use super::*;

// -------------------------------------------------------------------------------------------------
// sRGB target programs (newly covered — the oracle now gamma-encodes on write, matching the ROP)
// -------------------------------------------------------------------------------------------------

/// (13) sRGB CLEAR: `LoadOp::Clear` an `Rgba8Srgb` target to a mid-range opaque colour. Both backends
/// gamma-ENCODE the clear into sRGB on write (linear 0.5 → 188, not 128). ±2 for the encode's last-ULP
/// rounding (the CPU rounds half-up; lavapipe's clear path agrees to within a step).
pub(super) fn gen_clear_srgb(seed: u64) -> Prog {
    let w = 3 + (seed % 6) as u32; // 3..=8
    let h = 2 + (seed % 5) as u32; // 2..=6
    let c = fcolor_opaque(seed);
    let cmds = vec![
        Cmd::CreateTexture(1, tex_fmt(w, h, TextureFormat::Rgba8Srgb)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: c,
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    Prog {
        seed,
        category: "clear_srgb",
        ops: vec!["BeginRenderPass", "EndRenderPass"],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 2,
        kernel: None,
    }
}

/// (14) sRGB DRAW: a flat opaque replace draw of a constant linear colour into an `Rgba8Srgb` target — the
/// linear→sRGB encode happens on the fragment write on both backends. ±2 (lavapipe's shader-write path
/// rounds linear 0.5 to 187 where the clear/theoretical value is 188 — the documented encode-rounding gap).
pub(super) fn gen_draw_srgb(seed: u64) -> Prog {
    let w = 4 + (seed % 5) as u32;
    let h = 4 + (seed % 4) as u32;
    let c = fcolor_opaque(seed);
    let vbytes: Vec<u8> = FS_TRI
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]]))
        .collect();
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let cmds = vec![
        Cmd::CreateTexture(1, tex_fmt(w, h, TextureFormat::Rgba8Srgb)),
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
                vertex_buffers: vec![pos2_color_layout()],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Srgb,
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
        category: "draw_srgb",
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
