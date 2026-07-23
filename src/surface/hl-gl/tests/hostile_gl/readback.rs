use super::*;

// ===================================================================================================
// glReadPixels — out-of-bounds region / huge extent → bounded, no OOB read, no unbounded alloc
// ===================================================================================================

/// BUG (fixed): `glReadPixels` allocated a packed region proportional to `w*h*bpp` before any bound check.
/// A huge extent is now `GL_INVALID_VALUE` (never allocated); a normal read still works.
#[test]
fn read_pixels_huge_extent_does_not_unbounded_alloc() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // ~2 billion × ~2 billion × 4 bytes — must be rejected, not allocated.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, i32::MAX, i32::MAX, GL_RGBA).unwrap();
    assert!(px.is_empty());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // A normal-sized read of the (empty) default framebuffer returns a zero-filled buffer, no error.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, 4, 4, GL_RGBA).unwrap();
    assert_eq!(px.len(), 4 * 4 * 4);
    assert!(px.iter().all(|&b| b == 0));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// A `glReadPixels` region reaching outside the render target reads back zeros for the out-of-bounds
/// texels (no OOB slice read, no panic), and a negative width is an empty no-op.
#[test]
fn read_pixels_out_of_bounds_region_is_zero_filled_no_oob() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // A region far outside a (nonexistent) rendered target: bounded to zeros, no OOB read.
    let px = readpixels::read_pixels(&mut c, &mut sink, -100, -100, 8, 8, GL_RGBA).unwrap();
    assert_eq!(px.len(), 8 * 8 * 4);
    assert!(px.iter().all(|&b| b == 0));
    // A non-positive extent is an empty no-op.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, -1, 4, GL_RGBA).unwrap();
    assert!(px.is_empty());
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}
