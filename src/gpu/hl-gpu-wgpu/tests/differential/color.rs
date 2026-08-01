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
        tol: Tolerance::Unorm(2),
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
        tol: Tolerance::Unorm(2),
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
        tol: Tolerance::Unorm(2),
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
        tol: Tolerance::Unorm(1),
        kernel: None,
    }
}


// -------------------------------------------------------------------------------------------------
// Float colour targets — compared as VALUES in ULPs of the plane's own encoding, not as bytes
// -------------------------------------------------------------------------------------------------

fn float_plane(seed: u64) -> TextureFormat {
    match seed % 3 {
        0 => TextureFormat::Rgba16Float,
        1 => TextureFormat::Rgba32Float,
        _ => TextureFormat::R32Float,
    }
}

/// (F) 32-BIT FLOAT CLEAR: `LoadOp::Clear` an `Rgba32Float` or `R32Float` target, no draw. EXACT, and it
/// holds — every seed agrees bit for bit.
///
/// The clear colour goes through no arithmetic on either side and no narrowing conversion, so bit-exact
/// is the honest position rather than an aspiration, and it is kept separate from the half-float clear
/// precisely so that one case's need for a tolerance does not silently buy this one the same latitude.
pub(super) fn gen_clear_float(seed: u64) -> Prog {
    let format = if seed % 2 == 0 {
        TextureFormat::Rgba32Float
    } else {
        TextureFormat::R32Float
    };
    let texel = format.bytes_per_texel().expect("a float colour plane");
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
        category: "clear_float",
        ops: vec!["BeginRenderPass", "EndRenderPass"],
        cmds,
        read: Read::Tex {
            id: 1,
            len: (w * h) as usize * texel,
        },
        tol: Tolerance::Ulps(0),
        kernel: None,
    }
}

/// (F+1) HALF-FLOAT CLEAR: `LoadOp::Clear` an `Rgba16Float` target, no draw. ONE ULP, and the tolerance
/// is the host driver's, not ours.
///
/// Started at exact, which is what found this. Two of eight seeds disagreed by exactly one ULP and the
/// executor was LOW every time — the signature of truncation rather than round-to-nearest. Recomputing
/// the four cases in exact rational arithmetic with ties-to-even settled which side was right, and it was
/// the oracle in all four: for a source of 94/255, the nearest half is `0x35e6` and lavapipe stored
/// `0x35e5`. So this is a defect in the host's software Vulkan driver's f32→f16 clear conversion, not in
/// this driver, and the one ULP is allowed here only to keep a known host quirk from masking real work.
///
/// The oracle's own encoder is NOT left unguarded by that allowance: it is pinned exactly by the
/// exhaustive round trip over all 65536 half patterns, by the ties-to-even case, and by the clear-packing
/// value tests, none of which involve the host at all. Had this started permissive, the truncation would
/// have looked like agreement and the encoder would have had no cross-check at all.
///
/// RETIRE THIS TOLERANCE when the host driver rounds correctly. It absorbs someone else's defect, which
/// means a genuine one-ULP regression in our own encoder would not show up HERE — that is why the three
/// host-free guards above matter, and why this is written down rather than left as slack whose reason
/// nobody remembers. Measured 2026-08-01 against lavapipe: two of eight seeds low by one ULP. Re-run at
/// `Ulps(0)`; if it passes, the host has been fixed and this should go back to exact rather than being
/// kept because it is passing.
pub(super) fn gen_clear_half(seed: u64) -> Prog {
    let mut prog = gen_clear_float(seed);
    let format = TextureFormat::Rgba16Float;
    let texel = format.bytes_per_texel().expect("a float colour plane");
    let w = 3 + (seed % 6) as u32;
    let h = 2 + (seed % 5) as u32;
    prog.cmds[0] = Cmd::CreateTexture(1, tex_fmt(w, h, format));
    prog.category = "clear_half";
    prog.read = Read::Tex {
        id: 1,
        len: (w * h) as usize * texel,
    };
    prog.tol = Tolerance::Ulps(1);
    prog
}

/// (F+2) FLOAT DRAW: a flat opaque replace draw into a float target, full-screen triangle. ONE ULP, and
/// this tolerance is OURS.
///
/// Every texel is fully covered by a constant colour, so nothing should stand between the vertex value
/// and the stored texel — and on the executor nothing does: it stores the source f32 bit for bit. The
/// oracle is the side that drifts. Its barycentric evaluation computes `a·w0 + b·w1 + c·w2`, which for
/// three identical corners is `a` only up to rounding, and the file header already records that last-ULP
/// weight difference as a tolerance source for byte targets — where unorm quantization hides it. A float
/// target has no quantization to hide it in, so the same known approximation becomes visible here as
/// exactly one ULP.
///
/// Left as a tolerance rather than special-cased away. Making the reference short-circuit a constant
/// colour would tune it to this test rather than make it more correct, and a reference bent to agree is
/// the failure mode this whole battery exists to avoid.
pub(super) fn gen_draw_float(seed: u64) -> Prog {
    let format = float_plane(seed);
    let texel = format.bytes_per_texel().expect("a float colour plane");
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
        category: "draw_float",
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
        tol: Tolerance::Ulps(1),
        kernel: None,
    }
}
