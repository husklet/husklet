use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;

// (1) COPY BUG REGRESSIONS — sub-region source stride + tight (bytes_per_row == 0)
// =================================================================================================

/// Seed a 2x2 RGBA8 texture with 4 distinct texels via a tight buffer→texture upload.
fn seed_2x2(id: u32, texels: [[u8; 4]; 4]) -> Vec<Cmd> {
    let mut data = Vec::new();
    for t in texels {
        data.extend_from_slice(&t);
    }
    vec![
        Cmd::CreateTexture(id, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
        Cmd::CreateBuffer(
            200,
            buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
        ),
        Cmd::WriteBuffer {
            id: 200,
            offset: 0,
            data,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 200,
                src_offset: 0,
                bytes_per_row: 8, // tight: 2px * 4bpt
                dst: id,
                mip: 0,
                width: 2,
                height: 2,
            }],
            signal: None,
        }),
    ]
}

#[test]
fn copy_texture_to_buffer_subregion_uses_texture_stride() {
    // Left column (width=1, height=2) of a 2x2 texture must read texel (0,0) then (0,1) — advancing by the
    // TEXTURE row stride, not the copy width. The pre-fix code stepped by copy-width and returned (0,0),(1,0).
    let texels = [
        [1, 2, 3, 4],
        [5, 6, 7, 8],
        [9, 10, 11, 12],
        [13, 14, 15, 16],
    ];
    let mut g = exec();
    let mut cmds = seed_2x2(1, texels);
    cmds.push(Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer {
            src: 1,
            mip: 0,
            width: 1,
            height: 2,
            dst: 1,
            dst_offset: 0,
            bytes_per_row: 4,
        }],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 8).unwrap();
    assert_eq!(
        out,
        [1, 2, 3, 4, 9, 10, 11, 12],
        "sub-region copy must use the texture row stride"
    );
}

#[test]
fn copy_texture_to_buffer_tight_bytes_per_row_zero() {
    // bytes_per_row == 0 means "tightly packed" on the destination. Pre-fix, a zero stride collapsed every
    // row onto dst_offset (last row wins); the full 2x2 plane must land intact.
    let texels = [
        [1, 2, 3, 4],
        [5, 6, 7, 8],
        [9, 10, 11, 12],
        [13, 14, 15, 16],
    ];
    let mut g = exec();
    let mut cmds = seed_2x2(1, texels);
    cmds.push(Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_DST)));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer {
            src: 1,
            mip: 0,
            width: 2,
            height: 2,
            dst: 1,
            dst_offset: 0,
            bytes_per_row: 0, // tight
        }],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 16).unwrap();
    assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
}

#[test]
fn copy_buffer_to_texture_tight_bytes_per_row_zero() {
    // Upload a 2x2 plane with bytes_per_row == 0 (tight). Pre-fix, a zero source stride re-read row 0 for
    // every row; the texture must contain the distinct rows.
    let src: Vec<u8> = (1..=16).collect();
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 0, // tight
                    dst: 1,
                    mip: 0,
                    width: 2,
                    height: 2,
                }],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, src);
}

#[test]
fn copy_texture_to_texture_subregion() {
    // Copy the 1x1 texel at (1,1) of a seeded 2x2 source into (0,0) of a fresh 2x2 dest. Only dest (0,0)
    // changes; the rest stays zero. Exercises the newly-implemented (previously silently-dropped) T2T op.
    let texels = [
        [1, 2, 3, 4],
        [5, 6, 7, 8],
        [9, 10, 11, 12],
        [13, 14, 15, 16],
    ];
    let mut g = exec();
    let mut cmds = seed_2x2(1, texels);
    cmds.push(Cmd::CreateTexture(
        2,
        tex(2, 2, TextureFormat::Rgba8Unorm, RT),
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToTexture {
            src: 1,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 1, y: 1, z: 0 },
            dst: 2,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: Extent3d {
                width: 1,
                height: 1,
                depth: 1,
            },
        }],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let px = g.read_texture(&s.resources, 2).unwrap();
    assert_eq!(
        &px[0..4],
        &[13, 14, 15, 16],
        "texel (1,1) of src lands at (0,0) of dst"
    );
    assert_eq!(&px[4..16], &[0u8; 12], "the rest of dst is untouched");
}

#[test]
fn blit_and_resolve_are_advertised_and_run() {
    // BlitTexture (scaled/filtered) is resampled by a textured-triangle draw (`blit.rs`), and ResolveTexture
    // (multisample averaging) is now implemented as a zero-draw render-pass resolve (`submit::resolve_texture`).
    // BOTH are advertised AND run. (This test previously asserted ResolveTexture was UN-advertised and any
    // submitted resolve was REJECTED — the FAIL-before proof that the op landed.)
    let mut g = exec();
    let caps = g.capabilities();
    assert!(
        caps.supports_command(etag::BLIT_TEXTURE),
        "BlitTexture IS implemented + advertised"
    );
    assert!(
        caps.supports_command(etag::RESOLVE_TEXTURE),
        "ResolveTexture IS now implemented + advertised"
    );
    assert!(
        caps.supports_command(etag::COPY_T2T),
        "CopyTextureToTexture IS implemented + advertised"
    );

    let sub = TextureSubresource::base();
    let o = Origin3d::default();

    // A 1x1 → 2x2 nearest blit must run cleanly and replicate the single source texel across the whole dest
    // (the op the old executor rejected as unimplemented — the before/after regression control).
    let texel = [7u8, 8, 9, 10];
    let mut cmds = vec![
        Cmd::CreateTexture(1, tex(1, 1, TextureFormat::Rgba8Unorm, RT)),
        Cmd::CreateTexture(2, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
        Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: texel.to_vec(),
        },
    ];
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::CopyBufferToTexture {
                src: 1,
                src_offset: 0,
                bytes_per_row: 4,
                dst: 1,
                mip: 0,
                width: 1,
                height: 1,
            },
            Enc::BlitTexture {
                src: 1,
                src_sub: sub,
                src_origin: o,
                src_extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                dst: 2,
                dst_sub: sub,
                dst_origin: o,
                dst_extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                filter: Filter::Nearest,
                mirror: Mirror::NONE,
            },
        ],
        signal: None,
    }));
    let s = run_batch(&mut g, &cmds);
    let px = g.read_texture(&s.resources, 2).unwrap();
    for (i, out) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            out, texel,
            "dest texel {i} must be the upscaled source texel {texel:?}"
        );
    }

    // ResolveTexture now RUNS: clear a 4× MSAA target (all samples take the clear color), resolve it into a
    // single-sample texture, and read back the resolved plane. With every sample equal, the average is the
    // clear color EXACTLY — proving the resolve executed and wrote the destination (not a silent no-op).
    let e = Extent3d {
        width: 2,
        height: 2,
        depth: 1,
    };
    // 2×2, 4× MSAA render target (RENDER_TARGET only — a multisampled texture cannot be copied, only resolved).
    let msaa = TextureDesc {
        width: 2,
        height: 2,
        depth: 1,
        mip_levels: 1,
        sample_count: 4,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET,
        label: String::new(),
    };
    let clear = [0.2_f32, 0.4, 0.6, 1.0];
    let want = [51u8, 102, 153, 255]; // clear * 255, exact
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(3, msaa),
            Cmd::CreateTexture(
                4,
                tex(
                    2,
                    2,
                    TextureFormat::Rgba8Unorm,
                    RT | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // Clear-only MSAA pass: every sample of every texel becomes `clear` (stored).
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 3,
                            load: LoadOp::Clear,
                            clear: clear.map(f64::from),
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                    // Resolve the whole MSAA target into the single-sample destination.
                    Enc::ResolveTexture {
                        src: 3,
                        src_sub: sub,
                        src_origin: o,
                        dst: 4,
                        dst_sub: sub,
                        dst_origin: o,
                        extent: e,
                    },
                ],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 4).unwrap();
    for (i, out) in px.chunks_exact(4).enumerate() {
        let got = [out[0], out[1], out[2], out[3]];
        assert!(
            (0..4).all(|k| (got[k] as i16 - want[k] as i16).abs() <= 1),
            "resolved texel {i} must be the (uniform-sample) clear color {want:?}, got {got:?}"
        );
    }

    // The op is genuinely validated, not a blind pass: resolving a SINGLE-sampled source is rejected.
    let bad = try_batch(
        &mut g,
        &[
            Cmd::CreateTexture(5, tex(2, 2, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(
                6,
                tex(
                    2,
                    2,
                    TextureFormat::Rgba8Unorm,
                    RT | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 5,
                    src_sub: sub,
                    src_origin: o,
                    dst: 6,
                    dst_sub: sub,
                    dst_origin: o,
                    extent: e,
                }],
                signal: None,
            }),
        ],
    );
    assert!(
        bad.is_err(),
        "resolving a non-multisampled source must be a clean typed error"
    );
}

// =================================================================================================
