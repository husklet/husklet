use super::*;

// -------------------------------------------------------------------------------------------------
// texture clear + readback
// -------------------------------------------------------------------------------------------------

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
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, [255, 0, 0, 255]);
}

#[test]
fn texture_clear_bgra8_channel_order() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, [0, 0, 255, 255]); // B, G, R, A
}

#[test]
fn texture_clear_fills_all_texels() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
    let px = g.read_texture(&s.resources, 1).unwrap();
    let green = [0u8, 255, 0, 255];
    for texel in px.chunks_exact(4) {
        assert_eq!(texel, green);
    }
}

#[test]
fn texture_copy_observes_an_earlier_render_pass_in_the_same_command_buffer() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
                            clear: [1.0, 0.0, 0.0, 1.0],
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

    assert_eq!(
        g.read_buffer(&s.resources, BufferId(1), 0, 4).unwrap(),
        [255, 0, 0, 255],
        "the copy must execute after the clear encoded before it"
    );
}

#[test]
fn texture_clear_midgray_rounds_to_128() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
            clear_pass(1, [0.5, 0.5, 0.5, 0.5]),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, [128, 128, 128, 128]);
}

#[test]
fn clear_rect_scopes_to_subrectangle() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                }],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(&px[0..4], &[255, 0, 0, 255]);
    assert_eq!(&px[4..16], &[0u8; 12]);
}

// -------------------------------------------------------------------------------------------------
// texture -> buffer readback copy
// -------------------------------------------------------------------------------------------------

#[test]
fn texture_clear_then_copy_to_buffer() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
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
    let out = g.read_buffer(&s.resources, BufferId(1), 0, 4).unwrap();
    assert_eq!(out, [0, 0, 255, 255]); // blue
}

// -------------------------------------------------------------------------------------------------
// layered / mipped ClearRect
// -------------------------------------------------------------------------------------------------

/// A four-layer 2D array texture, initialised so every layer is distinguishable, cleared over layers
/// 1..=2 only. The layers named must change and the layers not named must not.
///
/// This is the case that failed 472 `dEQP-VK.api.image_clearing` cases: the executor passed `z = 0`,
/// `depth = 1` to `write_region` regardless of the requested range, so the clear landed on layer 0. A
/// regression to that behaviour makes layer 0 red and leaves layers 1 and 2 untouched — the exact
/// inversion the CTS reported as `Ref:(0,0,0,0) … Color:(<the clear colour>)`.
#[test]
fn clear_rect_writes_the_named_layers_and_only_those() {
    let mut g = exec();
    let layers = 4u32;
    let mut desc = tex(
        1,
        1,
        TextureFormat::Rgba8Unorm,
        texture_usage::COPY_DST | texture_usage::COPY_SRC,
    );
    desc.depth = layers;
    // One green texel per layer, so "untouched" is a positive value rather than the zero a failed
    // upload would also produce.
    let seed: Vec<u8> = (0..layers).flat_map(|_| [0u8, 255, 0, 255]).collect();
    let mut cmds = vec![
        Cmd::CreateTexture(1, desc),
        Cmd::CreateBuffer(
            2,
            buf(
                (4 * layers) as u64,
                buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: seed,
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTextureRegion {
                src: 2,
                src_offset: 0,
                bytes_per_row: 4,
                rows_per_image: 1,
                dst: 1,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d { x: 0, y: 0, z: 0 },
                extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: layers,
                },
            }],
            signal: None,
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 1,
                layer_count: 2,
                mip_level: 0,
            }],
            signal: None,
        }),
    ];
    // Read every layer back into its own slot of the staging buffer.
    for layer in 0..layers {
        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyTextureToBufferRegion {
                src: 1,
                src_sub: TextureSubresource {
                    mip: 0,
                    layer,
                    aspect: TextureAspect::All,
                },
                src_origin: Origin3d { x: 0, y: 0, z: 0 },
                extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                dst: 2,
                dst_offset: (layer * 4) as u64,
                bytes_per_row: 4,
                rows_per_image: 1,
            }],
            signal: None,
        }));
    }
    let s = run_batch(&mut g, &cmds);
    let out = g
        .read_buffer(&s.resources, BufferId(2), 0, (4 * layers) as usize)
        .unwrap();
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    assert_eq!(&out[0..4], &GREEN, "layer 0 was not in the range");
    assert_eq!(&out[4..8], &RED, "layer 1 was in the range");
    assert_eq!(&out[8..12], &RED, "layer 2 was in the range");
    assert_eq!(&out[12..16], &GREEN, "layer 3 was not in the range");
}

/// The same for mip levels: a clear naming level 1 must not touch level 0. The executor used to pass
/// `mip = 0` unconditionally, so a level-1 clear painted level 0.
#[test]
fn clear_rect_writes_the_named_mip_level_and_only_that() {
    let mut g = exec();
    let mut desc = tex(
        2,
        2,
        TextureFormat::Rgba8Unorm,
        texture_usage::COPY_DST | texture_usage::COPY_SRC,
    );
    desc.mip_levels = 2;
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, desc),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: vec![
                    0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
                ],
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTextureRegion {
                    src: 2,
                    src_offset: 0,
                    bytes_per_row: 8,
                    rows_per_image: 2,
                    dst: 1,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d { x: 0, y: 0, z: 0 },
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ClearRect {
                    texture: 1,
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                    color: [1.0, 0.0, 0.0, 1.0],
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 1,
                }],
                signal: None,
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToBufferRegion {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d { x: 0, y: 0, z: 0 },
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                    dst: 2,
                    dst_offset: 0,
                    bytes_per_row: 8,
                    rows_per_image: 2,
                }],
                signal: None,
            }),
        ],
    );
    let out = g.read_buffer(&s.resources, BufferId(2), 0, 16).unwrap();
    assert_eq!(
        out,
        vec![0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255],
        "a clear of mip 1 must leave mip 0 green"
    );
}
