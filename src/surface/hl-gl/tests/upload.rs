use hl_gl::model::context::PixelStore;
use hl_gl::model::glconst::*;
use hl_gl::service::upload::Upload;

#[test]
fn rgb565_glyph_atlas_upload_expands_exact_channels() {
    let upload = Upload::new(GL_RGB, GL_UNSIGNED_SHORT_5_6_5, 3, 1, PixelStore::default()).unwrap();
    let pixels = [0x00, 0xf8, 0xe0, 0x07, 0x1f, 0x00]; // native-endian red, green, blue on little-endian guests
    assert_eq!(upload.source_len(), 6);
    assert_eq!(
        upload.rgba8(&pixels).unwrap(),
        [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]
    );
}

#[test]
fn unpack_row_length_alignment_and_skips_select_the_exact_rectangle() {
    let store = PixelStore {
        unpack_alignment: 4,
        unpack_row_length: 3,
        unpack_skip_rows: 1,
        unpack_skip_pixels: 1,
        ..PixelStore::default()
    };
    let upload = Upload::new(GL_RED, GL_UNSIGNED_BYTE, 2, 2, store).unwrap();
    // Three one-byte pixels per row, padded to four. Skip row 0 and pixel 0 of rows 1 and 2.
    let source = [9, 9, 9, 0, 8, 10, 20, 0, 7, 30, 40];
    assert_eq!(upload.source_len(), source.len());
    assert_eq!(
        upload.rgba8(&source).unwrap(),
        [10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255, 40, 0, 0, 255]
    );
}

#[test]
fn short_or_unsupported_uploads_fail_without_partial_pixels() {
    let upload = Upload::new(GL_RGBA, GL_UNSIGNED_BYTE, 2, 1, PixelStore::default()).unwrap();
    assert!(upload.rgba8(&[0; 7]).is_none());
    assert!(Upload::new(GL_RGB, 0xdead, 2, 1, PixelStore::default()).is_none());
    assert!(Upload::new(GL_RGB, GL_UNSIGNED_BYTE, -1, 1, PixelStore::default()).is_none());
}

/// ES 3.0 §2.3.5: a `bits`-wide unsigned normalized integer is `c / (2^bits − 1)`, and its 8-bit form is
/// `round(f × 255)` — a ROUND, not a truncation.
///
/// The expansion was `c * 255 / max` in integer arithmetic, which truncates. The differential harness
/// caught it on a 6-bit green of 57: `57 * 255 / 63 = 14535 / 63 = 230.714`, which must give 231 and gave
/// 230. It is invisible wherever truncation and rounding coincide, which is why a 565 texel could be
/// wrong in green while its 5-bit red and blue agreed — so the arithmetic below is stated per channel
/// rather than checked on one sample.
#[test]
fn packed_565_expands_each_channel_by_the_spec_rounding() {
    let upload = Upload::new(GL_RGB, GL_UNSIGNED_SHORT_5_6_5, 1, 1, PixelStore::default()).unwrap();
    // r = 17 (5 bits), g = 57 (6 bits), b = 3 (5 bits)  →  0b10001_111001_00011 = 0x8F23.
    let packed: u16 = (17 << 11) | (57 << 5) | 3;
    // 17/31×255 = 139.84 → 140.   57/63×255 = 230.71 → 231.   3/31×255 = 24.68 → 25.
    assert_eq!(
        upload.rgba8(&packed.to_ne_bytes()).unwrap(),
        [140, 231, 25, 255]
    );
}

/// RGBA4, RGB5_A1 and RGB10_A2 uploads were REFUSED outright (`Upload::new` returned `None` for their
/// packed types), so the texture kept its zeroed storage while `glTexImage2D` reported no error — every
/// such texture sampled as transparent black. Component order is ES 3.0 table 3.2: MSB-first for the two
/// short types, LSB-first for the `_REV` int type.
#[test]
fn packed_4444_5551_and_2101010_rev_decode_in_spec_component_order() {
    let rgba4 = Upload::new(
        GL_RGBA,
        GL_UNSIGNED_SHORT_4_4_4_4,
        1,
        1,
        PixelStore::default(),
    )
    .unwrap();
    // r=1 g=2 b=4 a=15 → 0x1_2_4_F. 1/15×255 = 17, 2/15×255 = 34, 4/15×255 = 68, 15/15×255 = 255.
    let packed: u16 = (1 << 12) | (2 << 8) | (4 << 4) | 15;
    assert_eq!(
        rgba4.rgba8(&packed.to_ne_bytes()).unwrap(),
        [17, 34, 68, 255]
    );

    let rgb5a1 = Upload::new(
        GL_RGBA,
        GL_UNSIGNED_SHORT_5_5_5_1,
        1,
        1,
        PixelStore::default(),
    )
    .unwrap();
    // r=31 g=0 b=16 a=1 → 31/31×255 = 255, 0, 16/31×255 = 131.6 → 132, and a one-bit alpha is 0 or 255.
    let packed: u16 = (31 << 11) | (16 << 1) | 1;
    assert_eq!(
        rgb5a1.rgba8(&packed.to_ne_bytes()).unwrap(),
        [255, 0, 132, 255]
    );

    let rgb10a2 = Upload::new(
        GL_RGBA,
        GL_UNSIGNED_INT_2_10_10_10_REV,
        1,
        1,
        PixelStore::default(),
    )
    .unwrap();
    // _REV puts red in the LOW bits: r=1023 g=512 b=0 a=2.
    // 1023/1023×255 = 255, 512/1023×255 = 127.6 → 128, 0, 2/3×255 = 170.
    let packed: u32 = 1023 | (512 << 10) | (2 << 30);
    assert_eq!(
        rgb10a2.rgba8(&packed.to_ne_bytes()).unwrap(),
        [255, 128, 0, 170]
    );
}

/// A packed type is legal only with the ONE format ES 3.0 table 3.2 pairs it with; the mismatch must stay
/// refused rather than being decoded as something else.
#[test]
fn a_packed_type_with_the_wrong_format_is_still_refused() {
    assert!(Upload::new(
        GL_RGB,
        GL_UNSIGNED_SHORT_4_4_4_4,
        1,
        1,
        PixelStore::default()
    )
    .is_none());
    assert!(Upload::new(
        GL_RGBA,
        GL_UNSIGNED_SHORT_5_6_5,
        1,
        1,
        PixelStore::default()
    )
    .is_none());
}
