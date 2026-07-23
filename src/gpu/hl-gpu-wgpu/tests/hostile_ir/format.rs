use super::*;

// =================================================================================================
// (4) MISMATCHED formats
// =================================================================================================

#[test]
fn copy_texture_to_texture_between_incompatible_formats_converts_not_rejects() {
    let Some(mut g) = exec() else { return };
    // R8 (1 byte/texel) → Rgba8 (4 bytes/texel): DIFFERENT texel layouts. GL permits this as a CONVERTING
    // copy (the red channel expands to (R,0,0,1)); the executor now routes a format mismatch through a
    // converting blit instead of rejecting it (previously `Invalid("… incompatible formats")`). Prove it
    // SUCCEEDS and leaves the executor healthy — the exact-conversion pixel checks live in `t2t_convert.rs`.
    let mut s = session(&g);
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(
            &mut s,
            &mut *g,
            0,
            &[
                Cmd::CreateTexture(1, tex(4, 4, TextureFormat::R8Unorm, RT)),
                Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: sub(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: sub(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    }],
                    signal: None,
                }),
            ],
        )
    }));
    match r {
        Err(_) => panic!("[c2t2t_convert] converting copy PANICKED"),
        Ok(Err(e)) => panic!("[c2t2t_convert] converting copy must succeed, got {e:?}"),
        Ok(Ok(_)) => {}
    }
    drop(s);
    assert_survives(&mut g, "c2t2t_convert");
}

#[test]
fn resolve_non_multisampled_source_is_invalid() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "resolve_non_msaa",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)), // single-sampled src
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

#[test]
fn resolve_format_mismatch_is_invalid() {
    let Some(mut g) = exec() else { return };
    // Multisampled src, single-sample dst, but different formats.
    let msaa = TextureDesc {
        sample_count: 4,
        ..tex(
            4,
            4,
            TextureFormat::Rgba8Unorm,
            texture_usage::RENDER_TARGET,
        )
    };
    hostile(
        &mut g,
        "resolve_fmt_mismatch",
        &[
            Cmd::CreateTexture(1, msaa),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Bgra8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}
