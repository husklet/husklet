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
