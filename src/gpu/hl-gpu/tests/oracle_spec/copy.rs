use super::*;

#[test]
fn copy_buffer_to_texture_lays_out_rows() {
    // 2x2 rgba8 texture <- 16 tight bytes. bytes_per_row=8 (2 texels * 4). Each texel is distinct so a
    // transposed / mis-strided copy would be caught.
    let src: Vec<u8> = (0..16u8).collect(); // texel(0,0)=0..4, (1,0)=4..8, (0,1)=8..12, (1,1)=12..16
    let (exec, s) = run(&[
        Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: src.clone(),
        },
        Cmd::CreateTexture(
            1,
            tex(
                2,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 1,
                src_offset: 0,
                bytes_per_row: 8,
                dst: 1,
                mip: 0,
                width: 2,
                height: 2,
            }],
            signal: None,
        }),
    ]);
    assert_eq!(
        readback(&exec, &s, 1, 16),
        src,
        "buffer bytes copied verbatim into the tight texture plane"
    );
}

// =================================================================================================
// 7. CopyTextureToTexture — a 1x1 sub-region moves from src origin to dst origin, leaving the rest untouched.
// =================================================================================================

#[test]
fn copy_texture_to_texture_moves_only_the_named_region() {
    // src 2x2 cleared to red; dst 2x2 left zeroed. Copy the 1x1 texel at src(0,0) -> dst(1,1). Only dst
    // texel (1,1) becomes red; the other three stay [0,0,0,0].
    let (exec, s) = run(&[
        Cmd::CreateTexture(
            1,
            tex(
                2,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateTexture(
            2,
            tex(
                2,
                2,
                TextureFormat::Rgba8Unorm,
                texture_usage::COPY_DST | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [1.0, 0.0, 0.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
                Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d { x: 0, y: 0, z: 0 },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d { x: 1, y: 1, z: 0 },
                    extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                },
            ],
            signal: None,
        }),
    ]);
    let px = readback(&exec, &s, 2, 16);
    assert_eq!(
        &px[0..12],
        &[0u8; 12],
        "the three unaddressed texels stay zeroed"
    );
    assert_eq!(
        &px[12..16],
        &[255, 0, 0, 255],
        "dst texel (1,1) received the copied red texel"
    );
}

// =================================================================================================
// 8. BlitTexture — nearest SELECTS a texel; linear AVERAGES neighbors. A 2x1 -> 1x1 downscale distinguishes.
// =================================================================================================
