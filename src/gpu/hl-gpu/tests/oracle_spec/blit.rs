use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;
use hl_gpu::protocol::model::enums::TextureAspect;

fn blit_downscale(filter: Filter) -> [u8; 4] {
    // src 2x1: texel(0,0)=red, texel(1,0)=blue (populated via CopyBufferToTexture). Blit 2x1 -> dst 1x1.
    // Nearest: dst centre maps to src x=1 -> BLUE. Linear: samples between the two -> [128,0,128,255].
    let mut src = vec![0u8; 8];
    src[0..4].copy_from_slice(&[255, 0, 0, 255]); // red
    src[4..8].copy_from_slice(&[0, 0, 255, 255]); // blue
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src,
        },
        Cmd::CreateTexture(
            1,
            tex(
                2,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 8,
                    dst: 1,
                    mip: 0,
                    width: 2,
                    height: 1,
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 2,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    filter,
                    mirror: Mirror::NONE,
                },
            ],
            signal: None,
        }),
    ]);
    readback(&exec, &s, 2, 4).try_into().unwrap()
}

#[test]
fn blit_nearest_selects_and_linear_averages() {
    assert_eq!(
        blit_downscale(Filter::Nearest),
        [0, 0, 255, 255],
        "nearest picks the src texel the centre lands on (blue)"
    );
    assert_eq!(
        blit_downscale(Filter::Linear),
        [128, 0, 128, 255],
        "linear averages the two src texels (red+blue)"
    );
}

#[test]
fn depth_blit_nearest_scales_values_and_preserves_outside_texels() {
    let encode = |values: &[f32]| {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let source = encode(&[0.1, 0.2, 0.3, 0.4]);
    let destination = encode(&[0.9; 4]);
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC)),
        Cmd::CreateBuffer(2, buf(16, buffer_usage::COPY_SRC)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: source },
        Cmd::WriteBuffer { id: 2, offset: 0, data: destination },
        Cmd::CreateTexture(1, tex(4, 1, TextureFormat::Depth32Float, texture_usage::COPY_SRC | texture_usage::COPY_DST)),
        Cmd::CreateTexture(2, tex(4, 1, TextureFormat::Depth32Float, texture_usage::COPY_SRC | texture_usage::COPY_DST)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 16, dst: 1, mip: 0, width: 4, height: 1 },
                Enc::CopyBufferToTexture { src: 2, src_offset: 0, bytes_per_row: 16, dst: 2, mip: 0, width: 4, height: 1 },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource { aspect: TextureAspect::DepthOnly, ..TextureSubresource::base() },
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d { width: 4, height: 1, depth: 1 },
                    dst: 2,
                    dst_sub: TextureSubresource { aspect: TextureAspect::DepthOnly, ..TextureSubresource::base() },
                    dst_origin: Origin3d { x: 1, y: 0, z: 0 },
                    dst_extent: Extent3d { width: 2, height: 1, depth: 1 },
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                },
            ],
            signal: None,
        }),
    ]);
    assert_eq!(readback(&exec, &s, 2, 16), encode(&[0.9, 0.2, 0.4, 0.9]));
}

#[test]
fn packed_depth_stencil_storage_is_not_an_external_copy_layout() {
    let texture = tex(1, 1, TextureFormat::Depth24PlusStencil8, texture_usage::COPY_DST);
    let submit = |encoder| Cmd::Submit(CommandBuffer { encoder, signal: None });
    let whole = try_run(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC)),
        Cmd::CreateTexture(1, texture.clone()),
        submit(vec![Enc::CopyBufferToTexture {
            src: 1, src_offset: 0, bytes_per_row: 8, dst: 1, mip: 0, width: 1, height: 1,
        }]),
    ]);
    assert!(matches!(whole, Err(hl_gpu::GpuError::Unsupported(_))));

    let region = try_run(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC)),
        Cmd::CreateTexture(1, texture),
        submit(vec![Enc::CopyBufferToTextureRegion {
            src: 1, src_offset: 0, bytes_per_row: 8, rows_per_image: 1, dst: 1,
            dst_sub: TextureSubresource::base(), dst_origin: Origin3d::default(),
            extent: Extent3d { width: 1, height: 1, depth: 1 },
        }]),
    ]);
    assert!(matches!(region, Err(hl_gpu::GpuError::Unsupported(_))));
}

#[test]
fn blit_uses_the_named_source_and_destination_mips() {
    let mut source_texture = tex(
        8,
        8,
        TextureFormat::Rgba8Unorm,
        texture_usage::COPY_SRC | texture_usage::COPY_DST,
    );
    source_texture.mip_levels = 4;
    let mut destination_texture = source_texture.clone();
    destination_texture.usage |= texture_usage::RENDER_TARGET;
    let red = [211, 17, 43, 255];
    let source = red.repeat(16);
    let (exec, s) = run(&[
        Cmd::CreateTexture(1, source_texture),
        Cmd::CreateTexture(2, destination_texture),
        Cmd::CreateBuffer(1, buf(source.len() as u64, buffer_usage::COPY_SRC)),
        Cmd::CreateBuffer(2, buf(16, buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: source,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTextureRegion {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 16,
                    rows_per_image: 4,
                    dst: 1,
                    dst_sub: TextureSubresource {
                        mip: 1,
                        ..TextureSubresource::base()
                    },
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource {
                        mip: 1,
                        ..TextureSubresource::base()
                    },
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource {
                        mip: 2,
                        ..TextureSubresource::base()
                    },
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                },
                Enc::CopyTextureToBufferRegion {
                    src: 2,
                    src_sub: TextureSubresource {
                        mip: 2,
                        ..TextureSubresource::base()
                    },
                    src_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                    dst: 2,
                    dst_offset: 0,
                    bytes_per_row: 8,
                    rows_per_image: 2,
                },
            ],
            signal: None,
        }),
    ]);
    let mut pixels = vec![0; 16];
    exec.read_buffer(&s.resources, hl_gpu::BufferId(2), 0, &mut pixels)
        .unwrap();
    assert_eq!(pixels, red.repeat(4));
}

#[test]
fn blit_uses_the_named_mip_and_array_layer_together() {
    let mut texture = tex(
        8,
        8,
        TextureFormat::Rgba8Unorm,
        texture_usage::COPY_SRC | texture_usage::COPY_DST,
    );
    texture.depth = 6;
    texture.mip_levels = 4;
    let cyan = [9, 181, 207, 255];
    let source = cyan.repeat(16);
    let layer = |mip| TextureSubresource {
        mip,
        layer: 5,
        ..TextureSubresource::base()
    };
    let (exec, s) = run(&[
        Cmd::CreateTexture(1, texture),
        Cmd::CreateBuffer(1, buf(source.len() as u64, buffer_usage::COPY_SRC)),
        Cmd::CreateBuffer(2, buf(16, buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: source,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTextureRegion {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 16,
                    rows_per_image: 4,
                    dst: 1,
                    dst_sub: layer(1),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: layer(1),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                    dst: 1,
                    dst_sub: layer(2),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                },
                Enc::CopyTextureToBufferRegion {
                    src: 1,
                    src_sub: layer(2),
                    src_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                    dst: 2,
                    dst_offset: 0,
                    bytes_per_row: 8,
                    rows_per_image: 2,
                },
            ],
            signal: None,
        }),
    ]);
    let mut pixels = vec![0; 16];
    exec.read_buffer(&s.resources, hl_gpu::BufferId(2), 0, &mut pixels)
        .unwrap();
    assert_eq!(pixels, cyan.repeat(4));
}

/// A MIRRORED blit — the reference side.
///
/// `Mirror` exists because an unsigned origin and extent cannot say "flipped", and both GL and Vulkan
/// express a flip by inverting a rect's bounds. Before it, a mirrored `glBlitFramebuffer` reached this
/// oracle (and the executor) as an ordinary blit and produced an unmirrored image on both. The asymmetric
/// 3x2 source below distinguishes every one of the four states: no two of NONE / x / y / both agree on it.
///
/// NEAREST at 1:1 is pure texel selection, so this is exact.
fn blit_mirrored(mirror: Mirror) -> Vec<u8> {
    // src 3x2, six distinct texels. Every value differs, so a wrong reflection cannot alias onto a right
    // one — the failure names which axis is wrong by which texel moved where.
    let src: Vec<u8> = (0..6u8)
        .flat_map(|i| [10 + i * 40, 200 - i * 30, 5 + i * 20, 255])
        .collect();
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(24, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src,
        },
        Cmd::CreateTexture(
            1,
            tex(
                3,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                3,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 12,
                    dst: 1,
                    mip: 0,
                    width: 3,
                    height: 2,
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 3,
                        height: 2,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 3,
                        height: 2,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                    mirror,
                },
            ],
            signal: None,
        }),
    ]);
    readback(&exec, &s, 2, 24)
}

/// The expected 3x2 plane: destination texel `(x, y)` holds source texel `(x', y')` reflected per axis.
fn mirrored_expectation(mirror: Mirror) -> Vec<u8> {
    let texel = |i: u8| [10 + i * 40, 200 - i * 30, 5 + i * 20, 255];
    let mut out = Vec::with_capacity(24);
    for y in 0..2u8 {
        for x in 0..3u8 {
            let sx = if mirror.x { 2 - x } else { x };
            let sy = if mirror.y { 1 - y } else { y };
            out.extend_from_slice(&texel(sy * 3 + sx));
        }
    }
    out
}

#[test]
fn blit_mirror_reflects_each_axis() {
    for mirror in [
        Mirror::NONE,
        Mirror {
            x: true,
            ..Mirror::NONE
        },
        Mirror {
            y: true,
            ..Mirror::NONE
        },
        Mirror {
            x: true,
            y: true,
            ..Mirror::NONE
        },
    ] {
        assert_eq!(
            blit_mirrored(mirror),
            mirrored_expectation(mirror),
            "a {mirror:?} blit must reflect the source rect on exactly those axes"
        );
    }
    // The four results are pairwise DISTINCT, which is what makes the assertions above evidence: an
    // implementation that ignored `mirror` entirely would return the unmirrored plane four times and
    // three of the four comparisons would fail. Stated here so the control is checked, not assumed.
    let planes: Vec<Vec<u8>> = [
        Mirror::NONE,
        Mirror {
            x: true,
            ..Mirror::NONE
        },
        Mirror {
            y: true,
            ..Mirror::NONE
        },
        Mirror {
            x: true,
            y: true,
            ..Mirror::NONE
        },
    ]
    .into_iter()
    .map(mirrored_expectation)
    .collect();
    for i in 0..planes.len() {
        for j in i + 1..planes.len() {
            assert_ne!(planes[i], planes[j], "expectations {i} and {j} must differ");
        }
    }
}

// =================================================================================================
// 9. ResolveTexture path — a multisample src averages its samples into the single-sample dst. No public
//    write path fills a multisample texture in the oracle, so the reachable case is the zero-sample
//    average (documented limitation); this pins that the resolve op DISPATCHES and produces the exact
//    per-sample mean (0) rather than erroring or leaving the dst untouched-but-garbage.
// =================================================================================================

#[test]
fn resolve_multisample_averages_samples_into_single_sample_dst() {
    let (exec, s) = run(&[
        Cmd::CreateTexture(
            1,
            tex_ms(1, 1, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::ResolveTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d::default(),
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
            }],
            signal: None,
        }),
    ]);
    // mean of four zero samples per channel = 0; the op ran (didn't error) and wrote the resolved plane.
    assert_eq!(
        readback(&exec, &s, 2, 4),
        vec![0, 0, 0, 0],
        "resolve wrote the per-sample mean into the dst"
    );
}

/// A LINEAR blit of a float plane interpolates VALUES, not the bytes of their encoding.
///
/// Reachability first, because that was the open question: nothing on the path restricts the format.
/// `BlitTexture` validation checks the subresource, the extents, the usage flags, the sample count and
/// that the two texel SIZES match — never what the texels mean — so a `Filter::Linear` blit of an
/// `Rgba16Float` plane is a legal command that reaches the sampler.
///
/// It was measured wrong before it was fixed. The sampler read every plane as `bpt` independent
/// unsigned-normalized channels, so this case averaged the two half encodings byte-wise: the mean of the
/// high bytes `0x3C` and `0x00` reassembled as `0x1E00`, giving 0.0059 where the answer is 0.5 — about
/// eighty-five times too small.
///
/// The ALPHA channel is the reason nothing noticed. Both source texels carry alpha 1.0, so averaging
/// their identical bytes returns the identical bytes, and alpha read back correct throughout. A test that
/// checked only the channel both texels agreed on would have called this path healthy, which is why the
/// red and blue channels here are deliberately opposed.
#[test]
fn a_linear_blit_of_a_float_plane_interpolates_values() {
    let h = |v: f32| hl_gpu::protocol::model::half::from_f32(v).to_le_bytes();
    let mut src = Vec::new();
    for texel in [[1.0f32, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]] {
        for channel in texel {
            src.extend_from_slice(&h(channel));
        }
    }
    let bytes = src.len() as u64;
    let (exec, s) = run(&[
        Cmd::CreateBuffer(
            1,
            buf(bytes, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src,
        },
        Cmd::CreateTexture(
            1,
            tex(
                2,
                1,
                TextureFormat::Rgba16Float,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                1,
                1,
                TextureFormat::Rgba16Float,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 16,
                    dst: 1,
                    mip: 0,
                    width: 2,
                    height: 1,
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 2,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    filter: Filter::Linear,
                    mirror: Mirror::NONE,
                },
            ],
            signal: None,
        }),
    ]);
    let mut out = vec![0u8; 8];
    exec.read_texture(&s.resources, TextureId(2), &mut out)
        .expect("the destination is readable");
    let got: Vec<f32> = out
        .chunks_exact(2)
        .map(|c| hl_gpu::protocol::model::half::to_f32(u16::from_le_bytes([c[0], c[1]])))
        .collect();
    assert_eq!(
        got,
        vec![0.5, 0.0, 0.5, 1.0],
        "the midpoint of the two texels, not the midpoint of their bytes"
    );
}

/// An INTEGER plane cannot reach the linear filter at all, because the oracle cannot create one.
///
/// `sample_bilinear` refuses an integer format — averaging raw integers has no defined meaning and both
/// GL and Vulkan forbid linear filtering of an integer texture — but that arm is UNREACHABLE DEFENCE
/// today, and saying so is the point of this test rather than a caveat on it. Integer formats are absent
/// from `COLOR_FORMATS`, so the oracle's capabilities do not advertise them and `CreateTexture` is
/// refused with `ResourceLimit("texture format")` before any filter is chosen.
///
/// Asserted as unreachable rather than left unmentioned, so that this fails loudly if it ever stops being
/// true. The capability doc explicitly contemplates someone adding an integer format to that set; if they
/// do, this test goes red and points them at the filter question they would otherwise not know to ask.
///
/// The unorm arms are the positive control: the same program, same filters, differing only in format,
/// must succeed — otherwise the refusals above would be measuring a broken blit rather than a capability
/// boundary.
#[test]
fn an_integer_plane_cannot_reach_the_linear_filter_because_it_cannot_be_created() {
    let program = |format: TextureFormat, filter: Filter| {
        vec![
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![10u8, 20, 30, 40, 50, 60, 70, 80],
            },
            Cmd::CreateTexture(
                1,
                tex(
                    2,
                    1,
                    format,
                    texture_usage::COPY_DST | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateTexture(
                2,
                tex(
                    1,
                    1,
                    format,
                    texture_usage::COPY_DST | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 1,
                        mip: 0,
                        width: 2,
                        height: 1,
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 2,
                            height: 1,
                            depth: 1,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                        filter,
                        mirror: Mirror::NONE,
                    },
                ],
                signal: None,
            }),
        ]
    };

    // The controls first: prove the program itself works, or the refusals below establish nothing.
    for filter in [Filter::Nearest, Filter::Linear] {
        assert!(
            try_run(&program(TextureFormat::Rgba8Unorm, filter)).is_ok(),
            "a unorm plane blits under {filter:?}"
        );
    }

    // The integer plane never gets as far as the filter — it is refused at texture creation, by the
    // capability set, identically for both filters.
    for filter in [Filter::Nearest, Filter::Linear] {
        let refused = try_run(&program(TextureFormat::Rgba8Uint, filter))
            .expect_err("the oracle advertises no integer texture format");
        assert!(
            matches!(refused, hl_gpu::GpuError::ResourceLimit("texture format")),
            "refused by the capability set, not by the filter: {refused:?} under {filter:?}"
        );
    }
}

/// A blit between DIFFERENT formats converts, and the two texel sizes need not match.
///
/// The contract here is the host's, established by measuring it. The wgpu executor blits by rendering —
/// it samples the source and writes the destination as a colour attachment — so a differing format is a
/// conversion through the sampler and the ROP, and a size change is fine. Measured directly on lavapipe,
/// it accepts `Rgba8Unorm` into `Bgra8Unorm`, `Rgba8Srgb`, `R32Float` and `Rgba16Float`, and accepts
/// `R8Unorm` into `Rgba8Unorm` and `Rgba16Float` into `Rgba8Unorm` across a size change.
///
/// This oracle disagreed in BOTH directions, which is the reason to pin it here rather than only in the
/// differential. It refused every size change, so it diverged on operations the executor performs
/// perfectly well. And on the pairs it did accept, its nearest filter copied the source texel's raw
/// bytes: `Rgba8Unorm` into `R32Float` is four bytes either way, so unsigned bytes were reinterpreted as
/// a float while the executor converted, and both backends ran and silently disagreed.
#[test]
fn a_blit_converts_between_formats_and_across_texel_sizes() {
    // A one-channel source widening into four: the executor samples R8 as (r, 0, 0, 1), so the
    // destination takes the red channel and an opaque alpha — and the sizes differ, which this oracle
    // used to refuse outright.
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(2, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![255u8, 0],
        },
        Cmd::CreateTexture(
            1,
            tex(
                2,
                1,
                TextureFormat::R8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                2,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 2,
                    dst: 1,
                    mip: 0,
                    width: 2,
                    height: 1,
                },
                Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 2,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 2,
                        height: 1,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                },
            ],
            signal: None,
        }),
    ]);
    let mut out = vec![0u8; 8];
    exec.read_texture(&s.resources, TextureId(2), &mut out)
        .expect("the widened destination is readable");
    assert_eq!(
        out,
        vec![255, 0, 0, 255, 0, 0, 0, 255],
        "a one-channel source widens to (r, 0, 0, 1), and the size change is legal"
    );
}

/// The nearest filter CONVERTS across formats rather than copying bytes, which is the half a same-size
/// pair hides.
///
/// `Rgba8Unorm` and `R32Float` are both four bytes, so a byte copy passes every length check and produces
/// a number. The source texel here is (255, 0, 0, 255) — red — which as a float plane must read 1.0, and
/// as a verbatim byte copy reads whatever `0xFF0000FF` happens to mean as an f32. That the two are wildly
/// different is the point: a same-size cross-format pair is exactly where a reinterpret survives review.
#[test]
fn a_nearest_blit_converts_rather_than_reinterpreting_a_same_size_texel() {
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![255u8, 0, 0, 255],
        },
        Cmd::CreateTexture(
            1,
            tex(
                1,
                1,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                1,
                1,
                TextureFormat::R32Float,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
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
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                    mirror: Mirror::NONE,
                },
            ],
            signal: None,
        }),
    ]);
    let mut out = vec![0u8; 4];
    exec.read_texture(&s.resources, TextureId(2), &mut out)
        .expect("the float destination is readable");
    let got = f32::from_le_bytes(out.clone().try_into().expect("four bytes"));
    assert_eq!(
        got,
        1.0,
        "a red unorm texel is 1.0 in a float plane; a byte copy would give {:e}",
        f32::from_le_bytes([255, 0, 0, 255])
    );
}
