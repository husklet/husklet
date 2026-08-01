use super::*;

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
        Cmd::CreateBuffer(1, buf(bytes, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
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
        got, 1.0,
        "a red unorm texel is 1.0 in a float plane; a byte copy would give {:e}",
        f32::from_le_bytes([255, 0, 0, 255])
    );
}
