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
fn three_dimensional_unpack_layout_selects_each_requested_image() {
    let store = PixelStore {
        unpack_alignment: 1,
        unpack_row_length: 2,
        unpack_image_height: 2,
        unpack_skip_images: 1,
        ..PixelStore::default()
    };
    let upload = Upload::new_3d(GL_RED, GL_UNSIGNED_BYTE, 1, 1, 2, store).unwrap();
    let source = [9, 9, 9, 9, 10, 0, 0, 0, 20];
    assert_eq!(upload.source_len(), source.len());
    assert_eq!(
        upload.rgba8(&source).unwrap(),
        [10, 0, 0, 255, 20, 0, 0, 255]
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

/// A `GL_FLOAT` source into a half-float plane keeps the values a float texture exists for.
///
/// Driven at the endpoints on purpose. Mid-range values survive the eight-bit round trip this replaces
/// closely enough to look right — 0.5 comes back as 0.501961 — so a test at 0.5 would pass against the
/// conversion that narrows first and prove nothing. Above 1.0 and below 0.0 are where the narrowing is
/// visible: it clamped both, which is the entire reason a half-float texture is asked for.
#[test]
fn a_float_source_reaches_a_half_float_plane_unclamped() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    let upload = Upload::new(GL_RGBA, GL_FLOAT, 2, 1, PixelStore::default()).unwrap();
    let mut source = Vec::new();
    for value in [4.0f32, -2.5, 0.0, 1.0, 65504.0, 0.5, -1.0, 2048.0] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    let plane = upload.plane(&source, TextureFormat::Rgba16Float).unwrap();
    assert_eq!(plane.len(), 2 * 8, "two half-float texels");

    let half = |index: usize| u16::from_le_bytes([plane[index * 2], plane[index * 2 + 1]]);
    assert_eq!(half(0), 0x4400, "4.0");
    assert_eq!(half(1), 0xc100, "-2.5 keeps its sign rather than clamping to zero");
    assert_eq!(half(2), 0x0000, "0.0");
    assert_eq!(half(3), 0x3c00, "1.0");
    assert_eq!(half(4), 0x7bff, "65504.0 is half's largest finite value, not 1.0");
    assert_eq!(half(5), 0x3800, "0.5");
    assert_eq!(half(6), 0xbc00, "-1.0");
    assert_eq!(half(7), 0x6800, "2048.0");

    // The control: the same source through the eight-bit conversion, showing what the endpoints cost.
    let narrowed = upload.rgba8(&source).unwrap();
    assert_eq!(narrowed[0], 255, "4.0 narrows to the top of the byte range");
    assert_eq!(narrowed[1], 0, "-2.5 narrows to zero");
    assert_eq!(narrowed[4], 255, "and 65504.0 is indistinguishable from 4.0");
}

/// Values beyond half's range saturate to infinity rather than wrapping to a small finite number, and
/// NaN stays NaN. A wrap here is invisible mid-range and catastrophic at the top, which is exactly the
/// shape of error a conversion test is for.
#[test]
fn half_float_encoding_saturates_at_its_own_range() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    let upload = Upload::new(GL_RGBA, GL_FLOAT, 1, 1, PixelStore::default()).unwrap();
    let mut source = Vec::new();
    for value in [1.0e30f32, -1.0e30, f32::NAN, 1.0e-9] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    let plane = upload.plane(&source, TextureFormat::Rgba16Float).unwrap();
    let half = |index: usize| u16::from_le_bytes([plane[index * 2], plane[index * 2 + 1]]);
    assert_eq!(half(0), 0x7c00, "+inf");
    assert_eq!(half(1), 0xfc00, "-inf");
    assert_eq!(half(2) & 0x7c00, 0x7c00, "NaN keeps the infinity exponent");
    assert_ne!(half(2) & 0x03ff, 0, "and a non-zero mantissa, so it is not an infinity");
    assert_eq!(half(3), 0x0000, "below the smallest subnormal, flushed to zero");
}

/// A `GL_HALF_FLOAT` source reaches a half-float plane as the values it carries.
///
/// This is the source type an application uploading into a half-float texture actually uses, and the one
/// the eight-bit conversion could not read at all: its component width is two bytes, `narrowed` has no
/// arm for it, and the per-format path then copied the whole multi-byte texel through verbatim. A
/// four-channel source is therefore the WRONG case to test with — the verbatim copy produces the same
/// eight bytes and the assertion passes while measuring nothing, which is what it did when first written.
/// A three-channel source separates them: the correct answer is four half-float channels with an opaque
/// alpha the source does not contain, and the byte copy produces six bytes of source.
#[test]
fn a_half_float_source_reaches_a_half_float_plane_as_values() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    let upload = Upload::new(GL_RGB, GL_HALF_FLOAT, 1, 1, PixelStore::default()).unwrap();
    let source: Vec<u8> = [0x4400u16, 0xc100, 0x7bff]
        .iter()
        .flat_map(|bits| bits.to_le_bytes())
        .collect();
    let plane = upload.plane(&source, TextureFormat::Rgba16Float).unwrap();
    assert_eq!(plane.len(), 8, "one RGBA half-float texel, not the three channels supplied");
    assert_eq!(&plane[..6], &source[..], "the three supplied channels pass through unchanged");
    assert_eq!(
        &plane[6..],
        &[0x00, 0x3c],
        "and the alpha the source does not carry is 1.0"
    );
}

/// An 8-bit destination is unaffected: `plane` routes it to the same conversion it always used. Without
/// this control the float assertions above would also pass against a conversion that emitted float texels
/// for everything, which would break every ordinary texture in the driver.
#[test]
fn an_eight_bit_destination_still_takes_the_eight_bit_conversion() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    let upload = Upload::new(GL_RGBA, GL_UNSIGNED_BYTE, 2, 1, PixelStore::default()).unwrap();
    let source = [1u8, 2, 3, 4, 5, 6, 7, 8];
    for destination in [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba8Srgb,
        TextureFormat::Bgra8Unorm,
        TextureFormat::R8Unorm,
    ] {
        assert_eq!(
            upload.plane(&source, destination).unwrap(),
            upload.rgba8(&source).unwrap(),
            "{destination:?} takes the RGBA8 plane"
        );
    }
}

/// A source whose components are not values a float plane can hold is REFUSED, not filled with zeros.
/// Paired with a source that must convert, because a `plane` that returned `None` for everything would
/// satisfy the refusal on its own.
#[test]
fn a_float_plane_refuses_a_source_it_cannot_read_as_values() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    // Control: an ordinary unsigned-byte source converts into the float plane.
    let control = Upload::new(GL_RGBA, GL_UNSIGNED_BYTE, 1, 1, PixelStore::default()).unwrap();
    assert_eq!(
        control
            .plane(&[255, 0, 0, 255], TextureFormat::Rgba16Float)
            .unwrap(),
        [0x00, 0x3c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3c],
        "an unsigned normalized byte of 255 is 1.0, which is 0x3c00"
    );

    // A packed type carries a bit field, and this conversion does not decode the two packed FLOAT types.
    let packed =
        Upload::new(GL_RGB, GL_UNSIGNED_INT_10F_11F_11F_REV, 1, 1, PixelStore::default()).unwrap();
    assert!(
        packed.plane(&[0; 4], TextureFormat::Rgba16Float).is_none(),
        "a packed float type is refused rather than guessed at"
    );
    assert!(
        packed.rgba8(&[0; 4]).is_some(),
        "and the same source is still accepted by the eight-bit plane, so the refusal is about the destination"
    );
}
