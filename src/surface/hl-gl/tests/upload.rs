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
