//! Unit tests for the ES3 client-side object families filled in this pass: sampler objects, occlusion /
//! transform-feedback QUERY objects, transform-feedback objects (+ varying capture), separate-shader
//! PROGRAM PIPELINE objects, and immutable texture storage / sub-image uploads + buffer-parameter queries.
//!
//! These families lower to NO GPU IR (a real driver emits no command for them), so a plain [`GlContext`]
//! driven through the `hl_gl::service::{es3,record,query}` seam is the whole fixture — no socket, no GPU,
//! no guest cdylib. They assert the observable object STATE the app polls back plus the honest GL errors.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{es3, query, record};

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 256, height: 128 };
    c
}

// ---- sampler objects -----------------------------------------------------------------------------

#[test]
fn sampler_gen_bind_and_parameter_round_trip() {
    let mut c = ctx();
    let s = es3::gen_sampler(&mut c);
    assert_ne!(s, 0, "glGenSamplers must hand out a non-zero name");
    // A merely-reserved name is not yet a sampler OBJECT (lazy instantiation, matching GL).
    assert!(!es3::is_sampler(&c, s));

    // Bind it to a unit — this instantiates the object; the binding round-trips.
    es3::bind_sampler(&mut c, 3, s);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(es3::is_sampler(&c, s), "a bound sampler is a created object");
    assert_eq!(c.samplers.binding(3), s);

    // A parameter set round-trips through the getter.
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32, GL_NEAREST as f32);
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32, GL_CLAMP_TO_EDGE as f32);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(es3::get_sampler_parameter(&mut c, s, GL_TEXTURE_MIN_FILTER), Some(GL_NEAREST as f32));
    assert_eq!(es3::get_sampler_parameter(&mut c, s, GL_TEXTURE_WRAP_S), Some(GL_CLAMP_TO_EDGE as f32));

    // Defaults on an untouched parameter (ES 3.0 sampler table): MAG filter defaults to LINEAR.
    assert_eq!(es3::get_sampler_parameter(&mut c, s, GL_TEXTURE_MAG_FILTER), Some(GL_LINEAR as f32));
}

#[test]
fn sampler_rejects_bad_enum_and_unknown_name() {
    let mut c = ctx();
    let s = es3::gen_sampler(&mut c);

    // An out-of-range enum value raises GL_INVALID_ENUM and leaves the object untouched.
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_MIN_FILTER, 0xDEAD, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    // A sampler name never handed out by glGenSamplers is GL_INVALID_OPERATION.
    es3::sampler_parameter(&mut c, 9999, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // Deleting a bound sampler unbinds it from its unit.
    es3::bind_sampler(&mut c, 1, s);
    es3::delete_sampler(&mut c, s);
    assert_eq!(c.samplers.binding(1), 0);
    assert!(!es3::is_sampler(&c, s));
}

// ---- query objects -------------------------------------------------------------------------------

#[test]
fn query_begin_end_lifecycle_and_result_round_trip() {
    let mut c = ctx();
    let q = es3::gen_query(&mut c);
    assert_ne!(q, 0);
    assert!(!es3::is_query(&c, q), "a reserved name is not yet a query object");

    // Begin makes it the active query for its target.
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, q);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(es3::is_query(&c, q));
    assert_eq!(es3::get_queryiv(&mut c, GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY), Some(q as i32));

    // A second begin on the same target while active is GL_INVALID_OPERATION.
    let q2 = es3::gen_query(&mut c);
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, q2);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // End clears the active slot; the result becomes available (deferred model completes synchronously).
    es3::end_query(&mut c, GL_ANY_SAMPLES_PASSED);
    assert_eq!(es3::get_queryiv(&mut c, GL_ANY_SAMPLES_PASSED, GL_CURRENT_QUERY), Some(0));
    assert_eq!(es3::get_query_objectuiv(&mut c, q, GL_QUERY_RESULT_AVAILABLE), Some(1));
    // No occlusion executor ⇒ a truthful zero sample count.
    assert_eq!(es3::get_query_objectuiv(&mut c, q, GL_QUERY_RESULT), Some(0));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn query_rejects_bad_target_and_unknown_id() {
    let mut c = ctx();
    // A non-query target is GL_INVALID_ENUM.
    es3::begin_query(&mut c, GL_ARRAY_BUFFER, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // Begin with a name never from glGenQueries is GL_INVALID_OPERATION.
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, 4242);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // Result of an unknown query is GL_INVALID_OPERATION.
    assert_eq!(es3::get_query_objectuiv(&mut c, 4242, GL_QUERY_RESULT), None);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

// ---- transform-feedback objects ------------------------------------------------------------------

#[test]
fn transform_feedback_state_machine_and_varyings() {
    let mut c = ctx();
    let tf = es3::gen_transform_feedback(&mut c);
    assert_ne!(tf, 0);
    assert!(!es3::is_transform_feedback(&c, tf), "reserved, not yet an object");

    es3::bind_transform_feedback(&mut c, GL_TRANSFORM_FEEDBACK, tf);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(es3::is_transform_feedback(&c, tf));

    // begin → pause → resume → end, all valid transitions.
    es3::begin_transform_feedback(&mut c, GL_TRIANGLES);
    assert!(c.transform_feedbacks.bound_obj().active);
    es3::pause_transform_feedback(&mut c);
    assert!(c.transform_feedbacks.bound_obj().paused);
    es3::resume_transform_feedback(&mut c);
    assert!(!c.transform_feedbacks.bound_obj().paused);
    es3::end_transform_feedback(&mut c);
    assert!(!c.transform_feedbacks.bound_obj().active);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A double-begin is GL_INVALID_OPERATION; a bad primitive mode is GL_INVALID_ENUM.
    es3::begin_transform_feedback(&mut c, GL_TRIANGLES);
    es3::begin_transform_feedback(&mut c, GL_TRIANGLES);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    es3::end_transform_feedback(&mut c);
    es3::begin_transform_feedback(&mut c, 0xBEEF);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

#[test]
fn transform_feedback_varyings_round_trip() {
    let mut c = ctx();
    // A linked program is required for glTransformFeedbackVaryings.
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n");
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0); }\n");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));

    let names = vec!["vColor".to_string(), "vNormal".to_string()];
    es3::transform_feedback_varyings(&mut c, prog, names, GL_INTERLEAVED_ATTRIBS);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(es3::transform_feedback_varying(&c, prog, 0).as_deref(), Some("vColor"));
    assert_eq!(es3::transform_feedback_varying(&c, prog, 1).as_deref(), Some("vNormal"));
    assert_eq!(es3::transform_feedback_varying(&c, prog, 2), None);

    // An unknown program is GL_INVALID_VALUE; a bad buffer mode is GL_INVALID_ENUM.
    es3::transform_feedback_varyings(&mut c, 9999, vec!["x".to_string()], GL_INTERLEAVED_ATTRIBS);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    es3::transform_feedback_varyings(&mut c, prog, vec!["x".to_string()], 0xBEEF);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

// ---- program pipeline objects --------------------------------------------------------------------

#[test]
fn program_pipeline_stage_binding() {
    let mut c = ctx();
    // A separable program to bind into a stage.
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n");
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0); }\n");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));

    let pipe = es3::gen_program_pipeline(&mut c);
    assert_ne!(pipe, 0);
    es3::bind_program_pipeline(&mut c, pipe);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(es3::is_program_pipeline(&c, pipe));

    es3::use_program_stages(&mut c, pipe, GL_VERTEX_SHADER_BIT | GL_FRAGMENT_SHADER_BIT, prog);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(es3::get_program_pipelineiv(&mut c, pipe, GL_VERTEX_SHADER), Some(prog as i32));
    assert_eq!(es3::get_program_pipelineiv(&mut c, pipe, GL_FRAGMENT_SHADER), Some(prog as i32));

    es3::active_shader_program(&mut c, pipe, prog);
    assert_eq!(es3::get_program_pipelineiv(&mut c, pipe, GL_ACTIVE_PROGRAM), Some(prog as i32));

    // An unknown pipeline is GL_INVALID_OPERATION.
    assert_eq!(es3::get_program_pipelineiv(&mut c, 9999, GL_ACTIVE_PROGRAM), None);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

// ---- immutable texture storage + sub-image + buffer parameters -----------------------------------

/// Bind a fresh texture to unit 0 and return its GL name.
fn bound_texture(c: &mut GlContext) -> u32 {
    let t = record::gen_texture(c);
    record::active_texture(c, GL_TEXTURE0);
    record::bind_texture(c, GL_TEXTURE_2D, t);
    t
}

#[test]
fn tex_storage_2d_sizes_and_seals_the_texture() {
    let mut c = ctx();
    let t = bound_texture(&mut c);

    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 64, 32);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    {
        let tex = c.textures.get(t).expect("texture exists");
        assert_eq!((tex.w, tex.h), (64, 32));
        assert!(tex.immutable, "glTexStorage2D makes the texture immutable");
        assert_eq!(tex.data.len(), 64 * 32 * 4, "the RGBA8 base plane is allocated");
    }

    // A second glTexStorage2D on an immutable texture is GL_INVALID_OPERATION.
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 16, 16);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // Bad levels / extent → GL_INVALID_VALUE; bad target → GL_INVALID_ENUM.
    let _ = bound_texture(&mut c);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 2, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    record::tex_storage_2d(&mut c, GL_TEXTURE_3D, 1, GL_RGBA, 8, 8);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

#[test]
fn tex_sub_image_2d_writes_into_allocated_storage() {
    let mut c = ctx();
    let t = bound_texture(&mut c);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, 4, 4);

    // Overwrite the top-left 2x2 with red.
    let red = [255u8, 0, 0, 255].repeat(2 * 2);
    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, 0, 0, 2, 2, &red);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    let tex = c.textures.get(t).unwrap();
    assert_eq!(&tex.data[0..4], &[255, 0, 0, 255], "pixel (0,0) is red");

    // An out-of-bounds sub-rect is GL_INVALID_VALUE.
    record::tex_sub_image_2d(&mut c, GL_TEXTURE_2D, 0, 3, 3, 4, 4, &red);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn get_buffer_parameteriv_reports_size_and_usage() {
    let mut c = ctx();
    let b = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, b);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 48], 0x88E4 /* GL_STATIC_DRAW */);

    assert_eq!(query::get_buffer_parameteriv(&c, GL_ARRAY_BUFFER, GL_BUFFER_SIZE), 48);
    assert_eq!(query::get_buffer_parameteriv(&c, GL_ARRAY_BUFFER, GL_BUFFER_USAGE), 0x88E4);
    // An unknown pname reads 0.
    assert_eq!(query::get_buffer_parameteriv(&c, GL_ARRAY_BUFFER, 0xBEEF), 0);
}

#[test]
fn copy_buffer_sub_data_copies_bytes_between_buffers() {
    let mut c = ctx();
    let src = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, src);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[1, 2, 3, 4, 5, 6, 7, 8], 0);

    let dst = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_COPY_WRITE_BUFFER, dst);
    record::buffer_data(&mut c, GL_COPY_WRITE_BUFFER, &[0u8; 8], 0);

    record::copy_buffer_sub_data(&mut c, GL_ARRAY_BUFFER, GL_COPY_WRITE_BUFFER, 2, 0, 4);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(&c.buffers.get(dst).unwrap().data[0..4], &[3, 4, 5, 6]);
}
