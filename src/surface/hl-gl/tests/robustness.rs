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
    c.set_surface(GlSurface {
        have: true,
        width: 320,
        height: 240,
    });
    c
}

fn link_sources(context: &mut GlContext, vertex: &str, fragment: &str) -> u32 {
    let vertex_shader = record::create_shader(context, GL_VERTEX_SHADER);
    record::shader_source(context, vertex_shader, vertex);
    record::compile_shader(context, vertex_shader);
    let fragment_shader = record::create_shader(context, GL_FRAGMENT_SHADER);
    record::shader_source(context, fragment_shader, fragment);
    record::compile_shader(context, fragment_shader);
    let program = record::create_program(context);
    record::attach_shader(context, program, vertex_shader);
    record::attach_shader(context, program, fragment_shader);
    program
}

#[test]
fn program_link_rejects_unrepresentable_uniform_interfaces_with_diagnostics() {
    let mut context = ctx();
    let unsupported = link_sources(
        &mut context,
        "void main(){ gl_Position = vec4(0.0); }",
        "struct Material { vec4 color; };\nuniform Material material;\nvoid main(){}",
    );
    assert!(!record::link_program(&mut context, unsupported));
    assert_eq!(
        query::get_programiv(&context, unsupported, GL_LINK_STATUS),
        GL_FALSE as i32
    );
    assert!(query::program_info_log(&context, unsupported).contains("Material"));

    let oversized = link_sources(
        &mut context,
        "void main(){ gl_Position = vec4(0.0); }",
        "uniform vec4 values[2049];\nvoid main(){}",
    );
    assert!(!record::link_program(&mut context, oversized));
    assert!(query::program_info_log(&context, oversized).contains("8196"));

    let mut samplers = String::new();
    for index in 0..17 {
        samplers.push_str(&format!("uniform sampler2D texture{index};\n"));
    }
    samplers.push_str("void main(){}\n");
    let too_many_samplers = link_sources(
        &mut context,
        "void main(){ gl_Position = vec4(0.0); }",
        &samplers,
    );
    assert!(!record::link_program(&mut context, too_many_samplers));
    assert!(query::program_info_log(&context, too_many_samplers).contains("17 samplers"));
}

#[test]
fn binding_nonzero_names_materializes_resources() {
    let mut c = ctx();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, 41);
    record::bind_texture(&mut c, GL_TEXTURE_2D, 42);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 43);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, 44);

    assert!(c.buffers.get(41).is_some());
    assert!(c.textures.get(42).is_some());
    assert!(record::is_framebuffer(&c, 43));
    assert!(record::is_renderbuffer(&c, 44));
}

#[test]
fn chromium_texture_copy_preserves_pixels_and_applies_flip() {
    let mut c = ctx();
    c.textures.ensure(1);
    c.textures.ensure(2);
    c.textures.image_2d(
        1,
        1,
        2,
        &[255, 0, 0, 255, 0, 0, 255, 255],
        hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
    );

    assert!(c.textures.copy(1, 2, true, false, false));
    let copied = c.textures.get(2).expect("copied texture");
    assert_eq!(copied.data.as_slice(), [0, 0, 255, 255, 255, 0, 0, 255]);
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
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 100000, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

/// BUG (fixed): `glCopyTexSubImage2D` with a huge offset/extent overflowed an i32 in the dst-fit guard
/// and panicked. Now it is a clean `GL_INVALID_VALUE`.
#[test]
fn copy_tex_sub_image_rejects_huge_offset_without_panicking() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
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
    let b = c.buffers.gen();
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
fn indexed_buffer_range_requires_size_alignment_and_live_bounds() {
    let mut c = ctx();
    let b = c.buffers.gen();
    record::bind_buffer(&mut c, GL_UNIFORM_BUFFER, b);
    record::buffer_data(&mut c, GL_UNIFORM_BUFFER, &[0; 512], 0);

    for (offset, size) in [(0, 0), (1, 16), (256, 257), (isize::MAX, 16)] {
        record::bind_buffer_range(&mut c, GL_UNIFORM_BUFFER, 0, b, offset, size);
        assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
        assert!(record::indexed_buffer_binding(&c, GL_UNIFORM_BUFFER, 0).is_none());
    }

    record::bind_buffer_range(&mut c, GL_UNIFORM_BUFFER, 0, b, 256, 256);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        record::indexed_buffer_binding(&c, GL_UNIFORM_BUFFER, 0)
            .map(|binding| (binding.offset, binding.size)),
        Some((256, 256))
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
    assert!(c.draws().is_empty(), "no invalid draw was recorded");
    // A zero count is a legal no-op (no error, no draw).
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 0, 4);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(c.draws().is_empty());
}

#[test]
fn framebuffer_texture_2d_error_matrix() {
    let mut c = ctx();
    let fbo = c.gen_framebuffer();
    let tex = c.textures.gen();
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
    assert_eq!(c.framebuffer_color_attachment(fbo), tex);
}

#[test]
fn generate_mipmap_validates_target_and_bound_texture() {
    let mut c = ctx();
    c.active_texture(GL_TEXTURE0);
    // Bad target.
    c.generate_mipmap(GL_ARRAY_BUFFER);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // No texture bound on the active unit.
    c.generate_mipmap(GL_TEXTURE_2D);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

#[test]
fn pixel_store_rejects_bad_alignment_and_leaves_state_unchanged() {
    let mut c = ctx();
    record::pixel_store(&mut c, GL_UNPACK_ALIGNMENT, 3); // not in {1,2,4,8}
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(
        c.pixel_store_state().unpack_alignment,
        4,
        "the default is unchanged after a rejected set"
    );
    // A negative row length is rejected.
    record::pixel_store(&mut c, GL_UNPACK_ROW_LENGTH, -1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A valid set sticks.
    record::pixel_store(&mut c, GL_PACK_ALIGNMENT, 8);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.pixel_store_state().pack_alignment, 8);
}

#[test]
fn pack_alignment_pads_between_readback_rows_but_not_after_the_last() {
    let mut c = ctx();
    let store = c.pixel_store_state();
    // The default alignment of 4 leaves a three-pixel RGB row (9 bytes) starting on the next multiple.
    assert_eq!(store.pack_stride(9), 12);
    assert_eq!(store.pack_size(9, 1), 9, "a single row carries no padding");
    assert_eq!(
        store.pack_size(9, 4),
        45,
        "three padded rows plus the unpadded last one"
    );
    // An alignment of 8 pads a row that four already satisfied — the case dEQP's align_8 readback fails
    // when the copy ignores the alignment and writes every row at the tightly packed offset.
    record::pixel_store(&mut c, GL_PACK_ALIGNMENT, 8);
    let store = c.pixel_store_state();
    assert_eq!(store.pack_stride(12), 16);
    assert_eq!(store.pack_size(12, 3), 44);
    // A row that already fills whole alignment units is never padded, whatever the alignment.
    assert_eq!(store.pack_stride(16), 16);
    assert_eq!(store.pack_size(16, 3), 48);
    record::pixel_store(&mut c, GL_PACK_ALIGNMENT, 1);
    let store = c.pixel_store_state();
    assert_eq!(store.pack_stride(9), 9, "alignment 1 packs tightly");
    assert_eq!(store.pack_size(9, 4), 36);
}

#[test]
fn tex_sub_image_2d_rejects_out_of_bounds_rect() {
    let mut c = ctx();
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
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
    let src = c.buffers.gen();
    let dst = c.buffers.gen();
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
    assert_eq!(c.buffers.get(dst).unwrap().data.as_slice(), &[0u8; 32]);
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
    assert!(!c.delete_buffer(999));
    assert!(!c.delete_texture(999));
    assert!(!record::is_vertex_array(&c, 999));
    assert!(!record::is_framebuffer(&c, 999));
    assert!(!record::is_renderbuffer(&c, 999));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn deleting_the_bound_texture_clears_the_binding() {
    let mut c = ctx();
    // The program half of this case previously asserted that deleting the CURRENT program unbinds it.
    // That is the opposite of ES 3.0 §7.3, which flags a still-current program for deletion and keeps it
    // current, so the assertion was pinning a defect in place. The specified behaviour is now pinned by
    // `tests/gles_object_lifetime.rs::deleting_the_current_program_only_flags_it_and_it_stays_usable`.
    let t = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    assert_eq!(c.texture_at(0), t);
    c.delete_texture(t);
    assert_eq!(
        c.texture_at(0),
        0,
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
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, -1, 4, GL_MAP_WRITE_BIT).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // No bound buffer.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 0, 4, GL_MAP_WRITE_BIT).is_none());
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
    c.delete_sync(777);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn sampler_and_query_object_error_edges() {
    let mut c = ctx();
    // Parameterizing an unknown sampler name.
    es3::sampler_parameter(&mut c, 55, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // A generated sampler with a bad enum value.
    let s = c.samplers.gen();
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
