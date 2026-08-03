use super::*;

// -------------------------------------------------------------------------------------------------
// stencil programs (newly covered — the oracle now models the stencil test/op + reference)
// -------------------------------------------------------------------------------------------------

/// The centred quad (two triangles) spanning NDC `[-0.5, 0.5]^2` as 6 `pos2` vertices — the stencil MARK
/// geometry. On an `n`x`n` target with `n` a multiple of 4 it rasterises to the clean `[n/4, 3n/4)` pixel
/// block on BOTH backends (no pixel centre lands on the `±0.5` edge), so the marked region is byte-identical.
const CQUAD: [(f32, f32); 6] = [
    (-0.5, -0.5),
    (0.5, -0.5),
    (-0.5, 0.5),
    (0.5, -0.5),
    (0.5, 0.5),
    (-0.5, 0.5),
];

/// The `pos2 @loc0 (0..8)` + `colour @loc1 (8..24)`, stride-24 vertex layout the `SEED_POS2_COLOR` forwarding
/// shader (and the CPU oracle's `stride >= 24` arm) both read.
fn stencil_face(cmp: u32, pass: u32) -> StencilFaceState {
    StencilFaceState {
        compare: cmp,
        fail_op: stencil_op::KEEP,
        depth_fail_op: stencil_op::KEEP,
        pass_op: pass,
    }
}

/// Two-pass stencil mark-then-test. Pass A marks the centred rect into the stencil plane (`REPLACE` the
/// reference under `ALWAYS`). Pass B re-clears the colour target to `bg`, LOADs (preserves) the stencil, and
/// draws a fullscreen `fg` triangle gated by `test_cmp` against the SAME reference. `test_cmp = EQUAL` draws
/// INSIDE the marked rect (`ref == stored` there); `GREATER` draws OUTSIDE it (`ref > stored` holds only on
/// the 0-cleared exterior). Both backends must produce the identical flat two-colour image — EXACT (tol 0).
fn stencil_prog(seed: u64, category: &'static str, test_cmp: u32) -> Prog {
    let n = 8 + 4 * (seed % 4) as u32; // 8,12,16,20 — a multiple of 4 keeps the marked-rect edges pixel-clean
    let reference = 1 + (seed % 5) as u32; // 1..=5 (mark writes it, test compares against it)
    let bg = fcolor_opaque(seed);
    let fg = fcolor_opaque(seed.wrapping_add(3));
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);

    let quad: Vec<u8> = CQUAD
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, fg[0], fg[1], fg[2], fg[3]]))
        .collect();
    let tri: Vec<u8> = FS_TRI
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, fg[0], fg[1], fg[2], fg[3]]))
        .collect();

    let ds = TextureFormat::Depth24PlusStencil8;
    let mark_depth = DepthState {
        format: ds,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: stencil_face(compare::ALWAYS, stencil_op::REPLACE),
        stencil_back: stencil_face(compare::ALWAYS, stencil_op::REPLACE),
        stencil_read_mask: 0xFF,
        stencil_write_mask: 0xFF,
        bias_constant: 0,
        bias_slope_scale: 0.0,
        bias_clamp: 0.0,
    };
    let test_depth = DepthState {
        format: ds,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: stencil_face(test_cmp, stencil_op::KEEP),
        stencil_back: stencil_face(test_cmp, stencil_op::KEEP),
        stencil_read_mask: 0xFF,
        stencil_write_mask: 0x00,
        bias_constant: 0,
        bias_slope_scale: 0.0,
        bias_clamp: 0.0,
    };
    let pipe = |id: u32, depth: DepthState| {
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
                vertex_buffers: vec![pos2_color_layout()],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: Some(depth),
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        )
    };
    let cmds = vec![
        Cmd::CreateTexture(1, tex(n, n)),
        Cmd::CreateTexture(2, ds_tex(n, n)),
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv,
        },
        Cmd::CreateBuffer(
            1,
            buf(
                quad.len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::CreateBuffer(
            2,
            buf(
                tri.len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: quad,
        },
        Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: tri,
        },
        pipe(1, mark_depth),
        pipe(2, test_depth),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                // Pass A — MARK: clear stencil to 0, REPLACE the reference under the centred rect.
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
                Enc::SetStencilReference { reference },
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
                Enc::Draw {
                    vertex_count: 6,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
                // Pass B — TEST: re-clear colour to bg, LOAD (preserve) the stencil, draw fullscreen fg gated.
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: bg.map(f64::from),
                        store: true,
                    }],
                    depth: Some(DepthAttachment {
                        texture: 2,
                        depth_load: LoadOp::Load,
                        stencil_load: LoadOp::Load,
                        clear_depth: 1.0,
                        clear_stencil: 0,
                    }),
                },
                Enc::SetPipeline(2),
                Enc::SetStencilReference { reference },
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
        category,
        ops: vec![
            "BeginRenderPass(depth)",
            "SetPipeline",
            "SetStencilReference",
            "SetVertexBuffer",
            "Draw",
            "EndRenderPass",
        ],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (n * n * 4) as usize,
        },
        tol: Tolerance::Unorm(0),
        kernel: None,
    }
}

/// (15) STENCIL EQUAL: mark then draw gated by `EQUAL` — fg fills the marked rect, bg elsewhere.
pub(super) fn gen_stencil_equal(seed: u64) -> Prog {
    stencil_prog(seed, "stencil_equal", compare::EQUAL)
}

/// (16) STENCIL GREATER: mark then draw gated by `GREATER` (`ref > stored`) — the COMPLEMENT: fg fills the
/// exterior (stored 0), bg the marked rect. Exercises a second compare + proves the gate both ways.
pub(super) fn gen_stencil_greater(seed: u64) -> Prog {
    stencil_prog(seed, "stencil_greater", compare::GREATER)
}

// The generator table.
