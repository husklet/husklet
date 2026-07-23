use super::*;

// =================================================================================================
// (2) OUT-OF-BOUNDS regions
// =================================================================================================

#[test]
fn copy_buffer_to_buffer_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2b_overhang",
        &[
            Cmd::CreateBuffer(1, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 0,
                    dst: 2,
                    dst_offset: 0,
                    size: 64,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_buffer_to_texture_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2t_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(
                1,
                buf(65536, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 0,
                    dst: 1,
                    mip: 0,
                    width: 64,
                    height: 64,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_buffer_to_texture_bad_mip_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2t_bad_mip",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(
                1,
                buf(4096, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 0,
                    dst: 1,
                    mip: 9,
                    width: 4,
                    height: 4,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_texture_to_texture_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "c2t2t_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d { x: 3, y: 3, z: 0 },
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    }, // 3+4 > 4
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn copy_texture_to_buffer_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "t2b_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateBuffer(
                1,
                buf(65536, buffer_usage::COPY_SRC | buffer_usage::COPY_DST),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToBuffer {
                    src: 1,
                    mip: 0,
                    width: 64,
                    height: 64,
                    dst: 1,
                    dst_offset: 0,
                    bytes_per_row: 0,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn fill_buffer_overhang_is_oob() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "fill_overhang",
        &[
            Cmd::CreateBuffer(1, buf(64, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: 60,
                    size: 32,
                    value: 0xdead_beef,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn fill_buffer_offset_size_overflow_is_oob() {
    let Some(mut g) = exec() else { return };
    // offset + size overflows u64: without a guard this is a debug arithmetic PANIC.
    hostile(
        &mut g,
        "fill_overflow",
        &[
            Cmd::CreateBuffer(1, buf(64, buffer_usage::COPY_DST)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::FillBuffer {
                    buffer: 1,
                    offset: u64::MAX - 2,
                    size: 8,
                    value: 0xff,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

#[test]
fn blit_into_smaller_target_is_oob() {
    let Some(mut g) = exec() else { return };
    // dst rect (0,0 .. 8x8) overhangs a 4x4 destination.
    hostile(
        &mut g,
        "blit_overhang",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 8,
                        height: 8,
                        depth: 1,
                    },
                    filter: Filter::Nearest,
                }],
                signal: None,
            }),
        ],
        is_oob,
    );
}

/// `ClearRect` is DEFINED to clamp an over-hanging rect to the covered sub-rectangle (matching the CPU
/// oracle), NOT to error. Assert the clamp: an over-hang fills only the in-bounds texels and leaves the
/// rest at the pre-clear value — and of course does not panic.
#[test]
fn clear_rect_overhang_clamps_not_errors() {
    let Some(mut g) = exec() else { return };
    let mut s = session(&g);
    // Pre-clear the whole 4x4 to black, then a red ClearRect at (2,2) size 8x8 that overhangs to the edge:
    // it must fill ONLY the 2x2 bottom-right corner, leaving the rest black.
    hl_gpu::runtime::submit(
        &mut s,
        &mut *g,
        0,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::ClearRect {
                        texture: 1,
                        x: 0,
                        y: 0,
                        w: 4,
                        h: 4,
                        color: [0.0, 0.0, 0.0, 1.0],
                    },
                    Enc::ClearRect {
                        texture: 1,
                        x: 2,
                        y: 2,
                        w: 8,
                        h: 8,
                        color: [1.0, 0.0, 0.0, 1.0],
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("an over-hanging ClearRect must clamp (a defined no-op past the edge), never error");
    let px = g.read_texture(&s.resources, 1).unwrap();
    for y in 0..4u32 {
        for x in 0..4u32 {
            let o = ((y * 4 + x) * 4) as usize;
            let got = [px[o], px[o + 1], px[o + 2], px[o + 3]];
            let want = if x >= 2 && y >= 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(got, want, "clamped ClearRect pixel ({x},{y})");
        }
    }
    drop(s);
    assert_survives(&mut g, "clear_rect_overhang_clamps");
}
