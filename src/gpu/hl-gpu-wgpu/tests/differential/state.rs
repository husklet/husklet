use super::*;

// -------------------------------------------------------------------------------------------------
// write-mask programs (newly covered — the oracle now honors `ColorTargetState.write_mask`)
// -------------------------------------------------------------------------------------------------

/// A flat opaque (replace, blend-disabled) fullscreen draw of `fg` into an `Rgba8Unorm` target that was
/// cleared to `bg` in the same pass, through a pipeline whose `write_mask` is `mask`. The clear ignores the
/// write mask (it is a fixed-function attachment clear, not a masked ROP write), so afterwards each channel
/// reads `fg` where `mask`'s bit is SET and the cleared `bg` where it is CLEAR. Both backends must produce
/// the identical two-source image EXACTLY (tol 0): the masked channels are the exact clear and the written
/// channels are a flat replace of an exact `k/255` constant (no half-way rounding case, so the CPU
/// half-up and the GPU half-to-even quantizers land on the same byte). Used with `mask = 0x7` (RGB, alpha
/// preserved) and `mask = 0x8` (alpha only, RGB preserved) — the two masks a `glColorMask` guest most
/// commonly sets.
fn mask_prog(seed: u64, category: &'static str, mask: u32) -> Prog {
    let w = 4 + (seed % 5) as u32; // 4..=8
    let h = 4 + (seed % 4) as u32; // 4..=7
    let bg = fcolor4(seed);
    let fg = fcolor4(seed.wrapping_add(21));
    let vbytes: Vec<u8> = FS_TRI
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, fg[0], fg[1], fg[2], fg[3]]))
        .collect();
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let cmds = vec![
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
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: mask,
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
                        clear: bg,
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
        category,
        ops: vec![
            "BeginRenderPass",
            "SetPipeline",
            "SetVertexBuffer",
            "Draw(write_mask)",
            "EndRenderPass",
        ],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (12) WRITE-MASK RGB: `write_mask = 0x7` — R,G,B written from the draw, ALPHA preserved from the clear.
pub(super) fn gen_draw_mask_rgb(seed: u64) -> Prog {
    mask_prog(seed, "draw_mask_rgb", 0x7)
}

/// (13) WRITE-MASK ALPHA: `write_mask = 0x8` — only ALPHA written from the draw, R,G,B preserved from the
/// clear. The complement of the RGB mask; together they prove every channel's mask bit is honored.
pub(super) fn gen_draw_mask_alpha(seed: u64) -> Prog {
    mask_prog(seed, "draw_mask_alpha", 0x8)
}

// -------------------------------------------------------------------------------------------------
// face-culling programs (newly covered — the oracle now honors `RenderPipelineDesc.cull`/`front_face`)
// -------------------------------------------------------------------------------------------------

/// A single flat fullscreen triangle of KNOWN winding (`tri`) drawn (replace, blend off) over a `bg` clear
/// through a pipeline with the given `front_face` + `cull`. If the pipeline culls this triangle's facing the
/// whole target stays `bg` (a pure attachment clear on both backends); otherwise it is filled with `fg` (a
/// flat replace of an exact `k/255` constant). Either way the oracle and executor must AGREE EXACTLY
/// (tol 0) — a cull-face / winding mismatch flips a whole target between `bg` and `fg`, a divergence far
/// larger than any tolerance.
/// The four generators below span {`FS_TRI`, `FS_TRI_REV`} × {CCW, CW} × {Front, Back} so both cull faces,
/// both windings, and both front-face conventions are exercised, with a mix of culled + drawn outcomes.
fn cull_prog(
    seed: u64,
    category: &'static str,
    tri: &[(f32, f32); 3],
    front_face: u32,
    cull: u32,
) -> Prog {
    let w = 4 + (seed % 5) as u32; // 4..=8
    let h = 4 + (seed % 4) as u32; // 4..=7
    let bg = fcolor_opaque(seed);
    let fg = fcolor_opaque(seed.wrapping_add(13));
    let vbytes: Vec<u8> = tri
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, fg[0], fg[1], fg[2], fg[3]]))
        .collect();
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let cmds = vec![
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
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull,
                front_face,
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
                        clear: bg,
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
        category,
        ops: vec![
            "BeginRenderPass",
            "SetPipeline(cull)",
            "SetVertexBuffer",
            "Draw",
            "EndRenderPass",
        ],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h * 4) as usize,
        },
        tol: 0,
        kernel: None,
    }
}

/// (14) CULL, CCW front + cull BACK, over `FS_TRI` (framebuffer-CW → back-facing under CCW) → CULLED.
pub(super) fn gen_cull_ccw_back(seed: u64) -> Prog {
    cull_prog(seed, "cull_ccw_back", &FS_TRI, 0, 2)
}

/// (15) CULL, CCW front + cull FRONT, over `FS_TRI` (back-facing under CCW) → NOT culled → DRAWN. Same
/// geometry + front_face as (14) but the opposite cull face, so the outcome flips — the executor and oracle
/// must agree on which of the pair draws.
pub(super) fn gen_cull_ccw_front(seed: u64) -> Prog {
    cull_prog(seed, "cull_ccw_front", &FS_TRI, 0, 1)
}

/// (16) CULL, CW front + cull FRONT, over `FS_TRI` (framebuffer-CW → front-facing under CW) → CULLED. Same
/// geometry as (14)/(15) but the CW front-face convention, proving `front_face` flips the facing.
pub(super) fn gen_cull_cw_front(seed: u64) -> Prog {
    cull_prog(seed, "cull_cw_front", &FS_TRI, 1, 1)
}

/// (17) CULL, CCW front + cull FRONT, over `FS_TRI_REV` (opposite winding → front-facing under CCW) →
/// CULLED. The reversed-winding counterpart of (15): identical pipeline, swapped geometry winding, opposite
/// outcome — the direct proof the facing decision follows the triangle's winding.
pub(super) fn gen_cull_rev_ccw_front(seed: u64) -> Prog {
    cull_prog(seed, "cull_rev_ccw_front", &FS_TRI_REV, 0, 1)
}
