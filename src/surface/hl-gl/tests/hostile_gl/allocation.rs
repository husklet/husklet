use super::*;

// ===================================================================================================
// oversized dims / out-of-range ranges → bounded, GL_INVALID_VALUE, NEVER an unbounded alloc
// ===================================================================================================

/// BUG (fixed): `glTexImage2D` with an over-max (or empty-pixel) extent allocated a multi-GiB zeroed
/// plane. Now an extent beyond `GL_MAX_TEXTURE_SIZE` (or negative) is `GL_INVALID_VALUE` before any
/// allocation; a within-limits upload still works.
#[test]
fn tex_image_2d_oversized_extent_does_not_unbounded_alloc() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);

    // 40000×40000 RGBA8 = 6.4 GiB of zeroed storage — must be rejected, not allocated.
    record::tex_image_2d(&mut c, 40000, 40000, &[]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(
        c.textures.get(t).unwrap().data.is_empty(),
        "no oversized plane was materialized"
    );
    // A negative extent is also rejected.
    record::tex_image_2d(&mut c, -1, 16, &[]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // A within-limits upload still works.
    record::tex_image_2d(&mut c, 4, 4, &[0xABu8; 64]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.textures.get(t).unwrap().data.len(), 64);
}

/// BUG (fixed): `glBufferSubData` with a huge/overflowing offset grew the buffer `Vec` unbounded (or
/// panicked on `offset + len`). Now an out-of-range range is `GL_INVALID_VALUE` and the buffer is
/// untouched; an in-range write still works.
#[test]
fn buffer_sub_data_out_of_range_does_not_unbounded_alloc() {
    let mut c = ctx();
    let b = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, b);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[7u8; 32], 0);

    // A near-usize::MAX offset must not grow the buffer to a multi-exabyte Vec (or overflow the add).
    record::buffer_sub_data(&mut c, GL_ARRAY_BUFFER, usize::MAX - 3, &[1u8; 8]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A range that reaches just past the end is rejected too, leaving the buffer at its original size.
    record::buffer_sub_data(&mut c, GL_ARRAY_BUFFER, 30, &[1u8; 8]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(
        c.buffers.get(b).unwrap().data.len(),
        32,
        "buffer size is unchanged after a rejected write"
    );
    assert_eq!(
        c.buffers.get(b).unwrap().data,
        vec![7u8; 32],
        "bytes are untouched"
    );

    // An in-range write still lands.
    record::buffer_sub_data(&mut c, GL_ARRAY_BUFFER, 8, &[0xEE; 4]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(&c.buffers.get(b).unwrap().data[8..12], &[0xEE; 4]);
}

/// BUG (fixed): `glMapBufferRange` with an out-of-range offset/length grew the buffer `Vec` unbounded.
/// Now an offset/length beyond the buffer size is `GL_INVALID_VALUE`; an in-range map still works.
#[test]
fn map_buffer_range_out_of_range_does_not_unbounded_alloc() {
    let mut c = ctx();
    let b = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, b);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0);

    // A multi-GiB length must not grow the 32-byte buffer.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 0, 1 << 40, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // An overflowing offset+length is rejected too.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, isize::MAX, 16, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // Just past the end is rejected.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 30, 4, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(
        c.buffers.get(b).unwrap().data.len(),
        32,
        "buffer size is unchanged after a rejected map"
    );

    // An in-range map still works.
    let mapped = map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 8, 4, 0);
    assert_eq!(mapped, Some((b, 8)));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// A huge `glBindBufferRange` index, a huge compute dispatch group, and a huge tex-storage extent are all
/// bounded `GL_INVALID_VALUE`s with no allocation (round-out of the record + service size guards).
#[test]
fn oversized_indexed_binding_dispatch_and_storage_are_bounded() {
    let mut c = ctx();
    let b = c.buffers.gen();
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, u32::MAX, b);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    let mut sink = RecordingSink::with_full_caps();
    compute::dispatch_compute(&mut c, &mut sink, u32::MAX, 1, 1).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(sink.batches.is_empty());

    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, i32::MAX, i32::MAX);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}
