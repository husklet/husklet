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

/// `glTexSubImage2D` into immutable storage failed with `GL_INVALID_VALUE` for thirty-two sized formats
/// because `Upload::new` accepted only `GL_UNSIGNED_BYTE` and the packed types. Every remaining ES 3.0
/// component type and the integer/depth formats are now accepted; the model still materializes an RGBA8
/// plane, so wider components are narrowed — an honest partial, and the reason each expectation below is
/// stated as arithmetic rather than as a round trip.
#[test]
fn integer_and_wide_component_uploads_are_accepted_not_refused() {
    // GL_RGBA_INTEGER + GL_UNSIGNED_BYTE: the value is numeric, so it passes through unchanged.
    let rgba8ui = Upload::new(
        GL_RGBA_INTEGER,
        GL_UNSIGNED_BYTE,
        1,
        1,
        PixelStore::default(),
    )
    .expect("RGBA8UI must be accepted");
    assert_eq!(rgba8ui.rgba8(&[7, 8, 9, 10]).unwrap(), [7, 8, 9, 10]);

    // GL_RED_INTEGER + GL_UNSIGNED_SHORT: an integer value clamps to the 8-bit plane, it is not scaled.
    let r16ui = Upload::new(
        GL_RED_INTEGER,
        GL_UNSIGNED_SHORT,
        1,
        1,
        PixelStore::default(),
    )
    .expect("R16UI must be accepted");
    assert_eq!(r16ui.rgba8(&200u16.to_le_bytes()).unwrap(), [200, 0, 0, 255]);
    assert_eq!(
        r16ui.rgba8(&5000u16.to_le_bytes()).unwrap(),
        [255, 0, 0, 255],
        "an integer past the plane's range clamps"
    );

    // GL_RGBA + GL_FLOAT: a normalized value scales by 255. 0.5 -> 127.5 -> 128 (round to nearest).
    let rgba32f = Upload::new(GL_RGBA, GL_FLOAT, 1, 1, PixelStore::default())
        .expect("RGBA32F must be accepted");
    let source = [0.0f32, 0.5, 1.0, 2.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(
        rgba32f.rgba8(&source).unwrap(),
        [0, 128, 255, 255],
        "out of range clamps rather than wrapping"
    );

    // Every remaining type/format pair the ES 3.0 sized formats use must at least construct.
    for (format, type_) in [
        (GL_RED, GL_BYTE),
        (GL_RG, GL_BYTE),
        (GL_RGB, GL_BYTE),
        (GL_RGBA, GL_BYTE),
        (GL_RED_INTEGER, GL_BYTE),
        (GL_RG_INTEGER, GL_UNSIGNED_BYTE),
        (GL_RGB_INTEGER, GL_UNSIGNED_BYTE),
        (GL_RED_INTEGER, GL_SHORT),
        (GL_RGBA_INTEGER, GL_SHORT),
        (GL_RED_INTEGER, GL_INT),
        (GL_RGBA_INTEGER, GL_INT),
        (GL_RED_INTEGER, GL_UNSIGNED_INT),
        (GL_RGBA_INTEGER, GL_UNSIGNED_INT),
        (GL_RED, GL_FLOAT),
        (GL_RG, GL_FLOAT),
        (GL_RGB, GL_FLOAT),
        (GL_DEPTH_COMPONENT, GL_UNSIGNED_SHORT),
        (GL_DEPTH_COMPONENT, GL_UNSIGNED_INT),
        (GL_DEPTH_COMPONENT, GL_FLOAT),
        (GL_RGBA_INTEGER, GL_UNSIGNED_INT_2_10_10_10_REV),
        (GL_RGB, GL_UNSIGNED_INT_5_9_9_9_REV),
        (GL_RGB, GL_UNSIGNED_INT_10F_11F_11F_REV),
    ] {
        assert!(
            Upload::new(format, type_, 2, 2, PixelStore::default()).is_some(),
            "format {format:#x} type {type_:#x} must be accepted"
        );
    }
}
