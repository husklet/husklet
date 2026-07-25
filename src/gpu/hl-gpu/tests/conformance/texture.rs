use super::*;

fn clear_pass(texture: u32, clear: [f32; 4]) -> Cmd {
    Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture,
                    load: LoadOp::Clear,
                    clear,
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
}

#[test]
fn texture_clear_rgba8_readback_red() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            clear_pass(1, [1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let mut px = [0u8; 4];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(px, [255, 0, 0, 255]);
}

#[test]
fn texture_clear_bgra8_channel_order() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Bgra8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            clear_pass(1, [1.0, 0.0, 0.0, 1.0]),
        ],
    );
    let mut px = [0u8; 4];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(px, [0, 0, 255, 255]); // B, G, R, A
}

#[test]
fn texture_clear_fills_all_texels() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    2,
                    2,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            clear_pass(1, [0.0, 1.0, 0.0, 1.0]),
        ],
    );
    let mut px = [0u8; 16];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    let green = [0u8, 255, 0, 255];
    for texel in px.chunks_exact(4) {
        assert_eq!(texel, green);
    }
}

#[test]
fn texture_clear_midgray_rounds_to_128() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET,
                ),
            ),
            clear_pass(1, [0.5, 0.5, 0.5, 0.5]),
        ],
    );
    let mut px = [0u8; 4];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(px, [128, 128, 128, 128]);
}

#[test]
fn clear_rect_scopes_to_subrectangle() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    2,
                    2,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ClearRect {
                    texture: 1,
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                    color: [1.0, 0.0, 0.0, 1.0],
                }],
                signal: None,
            }),
        ],
    );
    let mut px = [0u8; 16];
    exec.read_texture(&s.resources, TextureId(1), &mut px)
        .unwrap();
    assert_eq!(&px[0..4], &[255, 0, 0, 255]);
    assert_eq!(&px[4..16], &[0u8; 12]);
}

// -------------------------------------------------------------------------------------------------
// texture -> buffer readback copy
// -------------------------------------------------------------------------------------------------

#[test]
fn texture_clear_then_copy_to_buffer() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let s = run_batch(
        &mut exec,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 1.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                    Enc::CopyTextureToBuffer {
                        src: 1,
                        mip: 0,
                        width: 1,
                        height: 1,
                        dst: 1,
                        dst_offset: 0,
                        bytes_per_row: 4,
                    },
                ],
                signal: None,
            }),
        ],
    );
    let mut out = [0u8; 4];
    exec.read_buffer(&s.resources, BufferId(1), 0, &mut out)
        .unwrap();
    assert_eq!(out, [0, 0, 255, 255]); // blue
}

// -------------------------------------------------------------------------------------------------
// FillBuffer — device-side memset (etag 21, WIRE_VERSION 5)
// -------------------------------------------------------------------------------------------------
