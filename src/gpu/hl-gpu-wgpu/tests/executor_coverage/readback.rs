use super::*;

// (2) READBACK REPACK — odd widths, strides, every supported color format (upload → tight readback)
// =================================================================================================

/// Upload `data` tightly into a fresh `w x h` texture of `fmt`, then read the tight plane back and assert
/// it round-trips byte-for-byte (exercises the padded→tight repack for the format's texel size + width).
fn roundtrip(fmt: TextureFormat, w: u32, h: u32, bpt: u32, data: Vec<u8>) {
    assert_eq!(data.len() as u32, w * h * bpt);
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(w, h, fmt, RT)),
            Cmd::CreateBuffer(
                1,
                buf(
                    data.len() as u64,
                    buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                ),
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: data.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: w * bpt,
                    dst: 1,
                    mip: 0,
                    width: w,
                    height: h,
                }],
                signal: None,
            }),
        ],
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!(px, data, "{fmt:?} {w}x{h} tight readback must round-trip");
}

#[test]
fn readback_rgba8_odd_width_3x2() {
    roundtrip(TextureFormat::Rgba8Unorm, 3, 2, 4, (0..24u8).collect());
}

#[test]
fn readback_rgba8_1x1() {
    roundtrip(TextureFormat::Rgba8Unorm, 1, 1, 4, vec![9, 8, 7, 6]);
}

#[test]
fn readback_rgba8_wide_100x1_crosses_256_stride() {
    roundtrip(
        TextureFormat::Rgba8Unorm,
        100,
        1,
        4,
        (0..400u32).map(|i| (i % 251) as u8).collect(),
    );
}

#[test]
fn readback_r8_width5() {
    roundtrip(TextureFormat::R8Unorm, 5, 1, 1, vec![10, 20, 30, 40, 50]);
}

#[test]
fn readback_rg8_3x1() {
    roundtrip(TextureFormat::Rg8Unorm, 3, 1, 2, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn readback_r32float_2x1() {
    let mut d = Vec::new();
    d.extend_from_slice(&0.25f32.to_le_bytes());
    d.extend_from_slice(&(-3.5f32).to_le_bytes());
    roundtrip(TextureFormat::R32Float, 2, 1, 4, d);
}

#[test]
fn readback_rgba16float_1x1() {
    // half-float bytes for (1.0, 0.0, 0.5, 1.0): 0x3C00, 0x0000, 0x3800, 0x3C00 (little-endian).
    roundtrip(
        TextureFormat::Rgba16Float,
        1,
        1,
        8,
        vec![0x00, 0x3C, 0x00, 0x00, 0x00, 0x38, 0x00, 0x3C],
    );
}

#[test]
fn readback_rgba32float_1x1() {
    let mut d = Vec::new();
    for v in [0.25f32, 0.5, 0.75, 1.0] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    roundtrip(TextureFormat::Rgba32Float, 1, 1, 16, d);
}

// =================================================================================================
