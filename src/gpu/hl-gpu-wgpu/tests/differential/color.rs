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

// -------------------------------------------------------------------------------------------------
// Narrow colour targets (newly covered — the oracle now renders into planes with fewer than four
// eight-bit channels, so this whole class of target is visible to the differential for the first time)
// -------------------------------------------------------------------------------------------------

/// (N) NARROW DRAW: a flat opaque replace draw into a one- or two-channel target.
///
/// This class was invisible. The oracle refused every draw whose target lacked a four-channel eight-bit
/// permutation, so no program here could render into `R8Unorm` or `Rg8Unorm` — and the refusal was the
/// ORACLE's, not the executor's, which shipped the formats. The cost was concrete: a `GL_R8` colour
/// attachment read back pure white through `glReadPixels` while this battery stayed green, because the
/// readback strided a one-byte plane at four bytes and nothing here could see a one-byte plane at all.
///
/// The channels the target does not have are the assertion, not a caveat. A one-channel plane keeps red
/// and drops green, blue and alpha; a reference that quietly wrote four channels into it would produce a
/// longer plane and fail on length before it failed on content.
///
/// Full-screen triangle, so every texel is fully covered and the comparison is flat-colour quantization
/// only — ±2, the same allowance its sRGB sibling takes for last-ULP rounding between the two paths.
pub(super) fn gen_draw_narrow(seed: u64) -> Prog {
    let format = if seed % 2 == 0 {
        TextureFormat::R8Unorm
    } else {
        TextureFormat::Rg8Unorm
    };
    let texel = format.bytes_per_texel().expect("a narrow colour plane");
    let w = 4 + (seed % 5) as u32;
    let h = 4 + (seed % 4) as u32;
    let c = fcolor_opaque(seed);
    let vbytes: Vec<u8> = FS_TRI
        .iter()
        .flat_map(|(x, y)| le_f32(&[*x, *y, c[0], c[1], c[2], c[3]]))
        .collect();
    let spirv = wgsl_to_spirv(SEED_POS2_COLOR);
    let cmds = vec![
        Cmd::CreateTexture(1, tex_fmt(w, h, format)),
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
                    format,
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
        category: "draw_narrow",
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
            len: (w * h) as usize * texel,
        },
        tol: 2,
        kernel: None,
    }
}

/// (N+1) NARROW CLEAR: `LoadOp::Clear` into a one- or two-channel target, no draw.
///
/// The clear path already served these formats — `clear_texel` has packed `R8Unorm` and `Rg8Unorm` since
/// before tonight — but nothing compared the two backends on one, because creating such a target meant
/// writing a program the oracle would refuse the moment anything was drawn. Cheap to add now that the
/// class is reachable, and it is the control for the draw above: if both fail, this says whether the
/// target or the draw is at fault.
pub(super) fn gen_clear_narrow(seed: u64) -> Prog {
    let format = if seed % 2 == 0 {
        TextureFormat::R8Unorm
    } else {
        TextureFormat::Rg8Unorm
    };
    let texel = format.bytes_per_texel().expect("a narrow colour plane");
    let w = 3 + (seed % 6) as u32;
    let h = 2 + (seed % 5) as u32;
    let c = fcolor_opaque(seed);
    let cmds = vec![
        Cmd::CreateTexture(1, tex_fmt(w, h, format)),
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
        category: "clear_narrow",
        ops: vec!["BeginRenderPass", "EndRenderPass"],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h) as usize * texel,
        },
        tol: 1,
        kernel: None,
    }
}

