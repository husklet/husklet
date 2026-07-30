use super::*;

// =================================================================================================
// (3) ZERO-SIZE / absurdly-huge dimensions
// =================================================================================================

#[test]
fn zero_width_texture_is_rejected() {
    let mut g = exec();
    hostile(
        &mut g,
        "zero_texture",
        &[Cmd::CreateTexture(
            1,
            tex(0, 4, TextureFormat::Rgba8Unorm, RT),
        )],
        is_rejected,
    );
}

#[test]
fn huge_texture_is_rejected() {
    let mut g = exec();
    hostile(
        &mut g,
        "huge_texture",
        &[Cmd::CreateTexture(
            1,
            tex(100_000, 100_000, TextureFormat::Rgba8Unorm, RT),
        )],
        is_rejected,
    );
}

#[test]
fn oversized_dispatch_is_oob() {
    let mut g = exec();
    let mut cmds = compute_setup();
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(10),
            Enc::SetBindGroup {
                index: 0,
                group: 10,
            },
            Enc::Dispatch {
                x: 4_000_000,
                y: 1,
                z: 1,
            },
            Enc::EndComputePass,
        ],
        signal: None,
    }));
    hostile(&mut g, "oversized_dispatch", &cmds, is_oob);
}

#[test]
fn zero_size_blit_is_invalid() {
    let mut g = exec();
    hostile(
        &mut g,
        "zero_blit",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 0,
                        height: 4,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                }],
                signal: None,
            }),
        ],
        is_invalid,
    );
}
