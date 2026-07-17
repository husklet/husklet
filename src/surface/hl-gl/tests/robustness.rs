//! Adversarial robustness: boundary values, invalid enums/handles, GL error-state transitions, and the
//! three overflow/consistency regressions fixed in this pass. Every op is driven to its error edge and the
//! REAL `glGetError` register (first-error-wins) is asserted — a bulletproofing gate for the recording +
//! object-lifecycle layer. Nothing here should ever panic.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{compute, es3, intro, map, query, record, sync};
use hl_gpu::RecordingSink;

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: 320,
        height: 240,
    };
    c
}

// ===================================================================================================
// REGRESSION: integer-overflow panics + reflection consistency (bugs fixed this pass)
// ===================================================================================================

/// BUG (fixed): `glRenderbufferStorage` with a huge extent overflowed an i32 in `Textures::image_2d`
/// (`w*h*4`) and panicked. Now it is a clean `GL_INVALID_VALUE` (beyond GL_MAX_RENDERBUFFER_SIZE).
#[test]
fn renderbuffer_storage_rejects_oversized_extent_without_panicking() {
    let mut c = ctx();
    let rb = record::gen_renderbuffer(&mut c);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rb);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA8, 40000, 40000);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // No backing plane was materialized: the reported extent stays zero.
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH),
        0
    );

    // A within-limits storage still works.
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA8, 256, 128);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH),
        256
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_HEIGHT),
        128
    );
}

/// BUG (fixed): `glTexStorage2D` above the advertised max is `GL_INVALID_VALUE` (was unbounded).
#[test]
fn tex_storage_2d_rejects_oversized_extent() {
    let mut c = ctx();
    let t = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 100000, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

/// BUG (fixed): `glCopyTexSubImage2D` with a huge offset/extent overflowed an i32 in the dst-fit guard
/// and panicked. Now it is a clean `GL_INVALID_VALUE`.
#[test]
fn copy_tex_sub_image_rejects_huge_offset_without_panicking() {
    let mut c = ctx();
    let t = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_image_2d(&mut c, 4, 4, &[0u8; 64]);
    record::copy_tex_sub_image_2d(
        &mut c,
        GL_TEXTURE_2D,
        0,
        2_000_000_000,
        0,
        0,
        0,
        2_000_000_000,
        1,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

/// BUG (fixed): a comment mentioning `attribute`/`in` used to be collected as a phantom attribute (only
/// `collect_vertex_attrs` skipped comment-stripping), shifting `glGetAttribLocation` away from the emitted
/// `layout(location)` namespace. Now the reflection and the emitted shader agree.
#[test]
fn attrib_location_namespace_matches_the_emitted_shader() {
    let vs = "/* attribute vec4 aLegacy; */\n// in vec2 aAlsoFake;\nattribute vec2 aPos;\nattribute vec3 aNrm;\n\
              void main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let mut c = ctx();
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, v, vs);
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, fs);
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    assert!(record::link_program(&mut c, p));
    assert_eq!(query::attrib_location(&c, p, "aPos"), 0);
    assert_eq!(query::attrib_location(&c, p, "aNrm"), 1);
    assert_eq!(
        query::attrib_location(&c, p, "aLegacy"),
        -1,
        "phantom attribute must not resolve"
    );
}

// ===================================================================================================
// GL error-state machine: first-error-wins across ops
// ===================================================================================================

#[test]
fn gl_error_is_first_error_wins_across_ops_then_clears() {
    let mut c = ctx();
    // First raise INVALID_ENUM (bad renderbuffer target), then INVALID_OPERATION (unbound rbo storage).
    record::bind_renderbuffer(&mut c, GL_TEXTURE_2D, 1); // bad target -> INVALID_ENUM
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA8, 4, 4); // no bound rbo -> INVALID_OPERATION
                                                                           // The FIRST error is retained.
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // And the register clears after a read.
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

// ===================================================================================================
// invalid enums / handles / boundary across the recording + service ops
// ===================================================================================================

#[test]
fn indexed_buffer_binding_validates_target_and_index() {
    let mut c = ctx();
    let b = record::gen_buffer(&mut c);
    // A non-indexed target is INVALID_ENUM.
    record::bind_buffer_range(&mut c, GL_ARRAY_BUFFER, 0, b, 0, 16);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // An index beyond the cap is INVALID_VALUE.
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, 100000, b);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A valid base binding records + reads back.
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, 2, b);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        record::indexed_buffer_binding(&c, GL_UNIFORM_BUFFER, 2).map(|x| x.buffer),
        Some(b)
    );
}

#[test]
fn draw_calls_reject_negative_counts_and_bad_ranges_recording_nothing() {
    let mut c = ctx();
    record::draw_elements_instanced(&mut c, GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, 0, -1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    record::draw_elements_base_vertex(&mut c, GL_TRIANGLES, -3, GL_UNSIGNED_INT, 0, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // glDrawRangeElements with end < start.
    record::draw_range_elements(&mut c, GL_TRIANGLES, 10, 3, 6, GL_UNSIGNED_SHORT, 0, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(c.draws.is_empty(), "no invalid draw was recorded");
    // A zero count is a legal no-op (no error, no draw).
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 0, 4);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(c.draws.is_empty());
}

#[test]
fn framebuffer_texture_2d_error_matrix() {
    let mut c = ctx();
    let fbo = record::gen_framebuffer(&mut c);
    let tex = record::gen_texture(&mut c);
    // Bad target -> INVALID_ENUM (checked first).
    record::framebuffer_texture_2d(
        &mut c,
        GL_ARRAY_BUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // Attaching to the DEFAULT framebuffer (none bound) -> INVALID_OPERATION.
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // Bind the FBO, then a bad level -> INVALID_VALUE.
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        3,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A valid attach succeeds.
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.framebuffers.color_attachment(fbo), tex);
}

#[test]
fn generate_mipmap_validates_target_and_bound_texture() {
    let mut c = ctx();
    record::active_texture(&mut c, GL_TEXTURE0);
    // Bad target.
    record::generate_mipmap(&mut c, GL_ARRAY_BUFFER);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // No texture bound on the active unit.
    record::generate_mipmap(&mut c, GL_TEXTURE_2D);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

#[test]
fn pixel_store_rejects_bad_alignment_and_leaves_state_unchanged() {
    let mut c = ctx();
    record::pixel_store(&mut c, GL_UNPACK_ALIGNMENT, 3); // not in {1,2,4,8}
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(
        c.pixel_store.unpack_alignment, 4,
        "the default is unchanged after a rejected set"
    );
    // A negative row length is rejected.
    record::pixel_store(&mut c, GL_UNPACK_ROW_LENGTH, -1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A valid set sticks.
    record::pixel_store(&mut c, GL_PACK_ALIGNMENT, 8);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.pixel_store.pack_alignment, 8);
}

#[test]
fn tex_sub_image_2d_rejects_out_of_bounds_rect() {
    let mut c = ctx();
    let t = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_image_2d(&mut c, 4, 4, &[0u8; 64]);
    // A rect that exceeds the texture bounds.
    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, 2, 2, 8, 8, &[0u8; 8 * 8 * 4]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A negative offset.
    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, -1, 0, 2, 2, &[0u8; 16]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn copy_buffer_sub_data_rejects_negative_and_no_ops_out_of_range() {
    let mut c = ctx();
    let src = record::gen_buffer(&mut c);
    let dst = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_COPY_READ_BUFFER, src);
    record::buffer_data(&mut c, GL_COPY_READ_BUFFER, &[1u8; 32], 0);
    record::bind_buffer(&mut c, GL_COPY_WRITE_BUFFER, dst);
    record::buffer_data(&mut c, GL_COPY_WRITE_BUFFER, &[0u8; 32], 0);
    // Negative size is INVALID_VALUE.
    record::copy_buffer_sub_data(&mut c, GL_COPY_READ_BUFFER, GL_COPY_WRITE_BUFFER, 0, 0, -4);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // Out-of-range read is a silent no-op (no error), leaving the destination untouched.
    record::copy_buffer_sub_data(&mut c, GL_COPY_READ_BUFFER, GL_COPY_WRITE_BUFFER, 30, 0, 8);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.buffers.get(dst).unwrap().data, vec![0u8; 32]);
    // A valid copy moves bytes.
    record::copy_buffer_sub_data(&mut c, GL_COPY_READ_BUFFER, GL_COPY_WRITE_BUFFER, 0, 0, 8);
    assert_eq!(&c.buffers.get(dst).unwrap().data[..8], &[1u8; 8]);
}

// ===================================================================================================
// object lifecycle: use-before-gen, delete-in-use, unlinked-program reflection
// ===================================================================================================

#[test]
fn deleting_unknown_objects_returns_false_and_no_error() {
    let mut c = ctx();
    assert!(!record::delete_buffer(&mut c, 999));
    assert!(!record::delete_texture(&mut c, 999));
    assert!(!record::is_vertex_array(&c, 999));
    assert!(!record::is_framebuffer(&c, 999));
    assert!(!record::is_renderbuffer(&c, 999));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn deleting_the_bound_program_and_texture_clears_the_binding() {
    let mut c = ctx();
    let p = record::create_program(&mut c);
    record::use_program(&mut c, p);
    assert_eq!(c.cur_prog, p);
    record::delete_program(&mut c, p);
    assert_eq!(c.cur_prog, 0, "deleting the current program unbinds it");

    let t = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    assert_eq!(c.tex_unit[0], t);
    record::delete_texture(&mut c, t);
    assert_eq!(
        c.tex_unit[0], 0,
        "deleting a bound texture clears the unit binding"
    );
}

#[test]
fn reflection_on_an_unlinked_program_is_empty_and_minus_one() {
    let mut c = ctx();
    let p = record::create_program(&mut c);
    // No link yet: locations resolve to -1, active reflection is None, statuses are false.
    assert_eq!(query::uniform_location(&c, p, "anything"), -1);
    assert_eq!(query::attrib_location(&c, p, "aPos"), -1);
    assert!(query::active_uniform(&c, p, 0).is_none());
    assert_eq!(query::get_programiv(&c, p, GL_LINK_STATUS), GL_FALSE as i32);
    // An unknown program name reports zeros / -1 too (never a panic).
    assert_eq!(query::get_programiv(&c, 424242, GL_LINK_STATUS), 0);
    assert_eq!(query::uniform_location(&c, 424242, "x"), -1);
}

// ===================================================================================================
// service ops (sink-touching) error edges
// ===================================================================================================

#[test]
fn dispatch_compute_out_of_range_group_is_an_error_and_submits_nothing() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // No compute program bound + an out-of-range group count: INVALID_VALUE, nothing submitted.
    compute::dispatch_compute(&mut c, &mut sink, 70000, 1, 1).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(sink.batches.is_empty());
}

#[test]
fn map_buffer_range_validates_range_and_binding() {
    let mut c = ctx();
    // Negative range.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, -1, 4, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // No bound buffer.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 0, 4, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

#[test]
fn fence_and_sync_error_edges() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // Bad condition / flags.
    assert!(sync::fence_sync(&mut c, &mut sink, 0xDEAD, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert!(sync::fence_sync(&mut c, &mut sink, GL_SYNC_GPU_COMMANDS_COMPLETE, 1).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // Waiting on / deleting an unknown sync.
    assert_eq!(
        sync::client_wait_sync(&mut c, &mut sink, 777, 0, 0),
        GL_WAIT_FAILED
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    sync::delete_sync(&mut c, 777);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn sampler_and_query_object_error_edges() {
    let mut c = ctx();
    // Parameterizing an unknown sampler name.
    es3::sampler_parameter(&mut c, 55, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // A generated sampler with a bad enum value.
    let s = es3::gen_sampler(&mut c);
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_MIN_FILTER, 0xDEAD, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // A valid parameter sticks.
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // Query: bad target then unknown id.
    es3::begin_query(&mut c, GL_ARRAY_BUFFER, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, 12345);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}
