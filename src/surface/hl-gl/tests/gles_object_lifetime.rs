//! Object-lifetime conformance: what GLES specifies must happen when an object is deleted while it is
//! bound, attached, or in use, and what the state must look like afterwards.
//!
//! Two different rules live here and are easy to conflate, which is why they are pinned side by side:
//!
//! * Buffers, textures, renderbuffers and framebuffers are deleted IMMEDIATELY and every binding /
//!   attachment naming them reverts to zero (ES 3.0 §2.9, §4.4.2.3, §4.4.1).
//! * Programs and shaders are NOT: deleting one that is still current or still attached only FLAGS it
//!   (`GL_DELETE_STATUS` becomes `GL_TRUE`) and it keeps working until it stops being referenced
//!   (ES 3.0 §7.1, §7.3). The "use then immediately release" idiom depends on this — Skia,
//!   `QOpenGLShaderProgram`'s destructor and glmark2's scene teardown all delete a program that is still
//!   current and then keep drawing with it.
//!
//! Driven in-process against `hl_gl::service::{record, query}`: these are pure object-state rules.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{query, record};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0); }\n";

fn ctx() -> GlContext {
    let mut context = GlContext::new();
    context.set_surface(GlSurface {
        have: true,
        width: 256,
        height: 256,
    });
    context
}

/// A linked program plus the two shaders it was built from.
fn linked(context: &mut GlContext) -> (u32, u32, u32) {
    let vertex = record::create_shader(context, GL_VERTEX_SHADER);
    record::shader_source(context, vertex, VS);
    record::compile_shader(context, vertex);
    let fragment = record::create_shader(context, GL_FRAGMENT_SHADER);
    record::shader_source(context, fragment, FS);
    record::compile_shader(context, fragment);
    let program = record::create_program(context);
    record::attach_shader(context, program, vertex);
    record::attach_shader(context, program, fragment);
    assert!(
        record::link_program(context, program),
        "the fixture program must link"
    );
    (program, vertex, fragment)
}

// ---- immediate deletion: buffers, textures, renderbuffers, framebuffers --------------------------

#[test]
fn deleting_a_bound_buffer_reverts_the_binding_to_zero() {
    let mut context = ctx();
    let buffer = context.buffers.gen();
    assert!(record::bind_buffer(&mut context, GL_ARRAY_BUFFER, buffer));
    let mut out = [0i32; 4];
    query::get_integerv(&context, GL_ARRAY_BUFFER_BINDING, &mut out);
    assert_eq!(out[0], buffer as i32);

    // ES 3.0 §2.9: deleting a bound buffer is as if glBindBuffer(target, 0) had been called for it.
    assert!(context.delete_buffer(buffer));
    query::get_integerv(&context, GL_ARRAY_BUFFER_BINDING, &mut out);
    assert_eq!(out[0], 0, "the ARRAY_BUFFER binding must revert to 0");
    assert_eq!(
        context.take_gl_error(),
        GL_NO_ERROR,
        "deleting a bound buffer is not an error"
    );
}

#[test]
fn deleting_a_bound_framebuffer_reverts_to_the_default_framebuffer() {
    let mut context = ctx();
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    let mut out = [0i32; 4];
    query::get_integerv(&context, GL_FRAMEBUFFER_BINDING, &mut out);
    assert_eq!(out[0], framebuffer as i32);

    // ES 3.0 §4.4.1: deleting the bound framebuffer binds framebuffer zero, so the following draw goes
    // to the window instead of to a dangling name.
    assert!(context.delete_framebuffer(framebuffer));
    query::get_integerv(&context, GL_FRAMEBUFFER_BINDING, &mut out);
    assert_eq!(out[0], 0, "the FRAMEBUFFER binding must revert to 0");
    query::get_integerv(&context, GL_READ_FRAMEBUFFER_BINDING, &mut out);
    assert_eq!(
        out[0], 0,
        "GL_FRAMEBUFFER binds both read and draw, so both must revert"
    );
}

#[test]
fn deleting_an_attached_renderbuffer_detaches_it_from_the_bound_framebuffer() {
    let mut context = ctx();
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    let renderbuffer = record::gen_renderbuffer(&mut context);
    record::bind_renderbuffer(&mut context, GL_RENDERBUFFER, renderbuffer);
    record::renderbuffer_storage(&mut context, GL_RENDERBUFFER, GL_RGBA8, 64, 64);
    record::framebuffer_renderbuffer(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_RENDERBUFFER,
        renderbuffer,
    );
    assert_eq!(
        context.framebuffer_color_attachment(framebuffer),
        renderbuffer,
        "the renderbuffer must be the framebuffer's color attachment"
    );

    // ES 3.0 §4.4.2.3: deleting a renderbuffer detaches it from every attachment point of the bound
    // framebuffer, as if glFramebufferRenderbuffer(..., 0) had been called.
    assert!(context.delete_renderbuffer(renderbuffer));
    assert_eq!(
        context.framebuffer_color_attachment(framebuffer),
        0,
        "a deleted renderbuffer must not remain an attachment of the bound framebuffer"
    );
    let mut out = [0i32; 4];
    query::get_integerv(&context, GL_RENDERBUFFER_BINDING, &mut out);
    assert_eq!(out[0], 0, "the RENDERBUFFER binding must revert to 0");
}

#[test]
fn deleting_an_attached_texture_detaches_it_from_the_bound_framebuffer() {
    let mut context = ctx();
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    record::tex_image_2d(&mut context, 64, 64, &[]);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );

    // ES 3.0 §4.4.2.3 again, for the texture attachment path.
    assert!(context.delete_texture(texture));
    assert_eq!(
        context.framebuffer_color_attachment(framebuffer),
        0,
        "a deleted texture must not remain an attachment of the bound framebuffer"
    );
    assert_eq!(
        context.texture_at(0),
        0,
        "the texture unit binding must revert to 0"
    );
}

// ---- deferred deletion: programs and shaders ------------------------------------------------------

#[test]
fn deleting_the_current_program_only_flags_it_and_it_stays_usable() {
    let mut context = ctx();
    let (program, _, _) = linked(&mut context);
    record::use_program(&mut context, program);
    assert_eq!(context.current_program(), program);

    // ES 3.0 §7.3: "If a program object is in use as the current program for the current rendering
    // context, it will be flagged for deletion, but it will not be deleted until it is no longer part of
    // the current rendering state." So the binding must survive and GL_DELETE_STATUS must become GL_TRUE.
    record::delete_program(&mut context, program);
    assert_eq!(
        context.current_program(),
        program,
        "deleting the CURRENT program must not unbind it; the release-then-draw idiom \
         (glUseProgram(p); glDeleteProgram(p); glDrawArrays(...)) draws nothing if it does"
    );
    assert_eq!(
        query::get_programiv(&context, program, GL_DELETE_STATUS),
        GL_TRUE as i32,
        "GL_DELETE_STATUS must report the pending deletion"
    );
    assert_eq!(
        query::get_programiv(&context, program, GL_LINK_STATUS),
        GL_TRUE as i32,
        "a flagged program is still linked and still queryable"
    );

    // Only when it stops being current is it actually gone.
    record::use_program(&mut context, 0);
    assert_eq!(
        query::get_programiv(&context, program, GL_LINK_STATUS),
        GL_FALSE as i32,
        "once no longer current, the flagged program is deleted"
    );
}

#[test]
fn deleting_a_program_that_is_not_current_takes_effect_at_once() {
    let mut context = ctx();
    let (program, _, _) = linked(&mut context);
    record::delete_program(&mut context, program);
    assert_eq!(
        query::get_programiv(&context, program, GL_LINK_STATUS),
        GL_FALSE as i32,
        "an unreferenced program is deleted immediately"
    );
    assert_eq!(
        context.take_gl_error(),
        GL_NO_ERROR,
        "deleting an unreferenced program is not an error"
    );
}

#[test]
fn deleting_an_attached_shader_keeps_it_attached_until_it_is_detached() {
    let mut context = ctx();
    let (program, vertex, fragment) = linked(&mut context);
    assert_eq!(
        query::get_programiv(&context, program, GL_ATTACHED_SHADERS),
        2
    );

    // ES 3.0 §7.1: deleting a shader still attached to a program flags it; the attachment survives until
    // glDetachShader. Every toolkit that deletes its shaders right after linking relies on this — losing
    // the attachment early makes a later glLinkProgram of the same program fail.
    record::delete_shader(&mut context, vertex);
    assert_eq!(
        query::get_shaderiv(&context, vertex, GL_DELETE_STATUS),
        GL_TRUE as i32,
        "GL_DELETE_STATUS must report the pending shader deletion"
    );
    assert_eq!(
        query::get_programiv(&context, program, GL_ATTACHED_SHADERS),
        2,
        "a flagged-but-attached shader is still attached"
    );
    assert!(
        record::link_program(&mut context, program),
        "the program must still relink from its attached shaders after they were deleted"
    );

    record::detach_shader(&mut context, program, vertex);
    record::detach_shader(&mut context, program, fragment);
    assert_eq!(
        query::get_programiv(&context, program, GL_ATTACHED_SHADERS),
        0
    );
}

#[test]
fn a_program_with_only_one_stage_attached_fails_to_link() {
    let mut context = ctx();
    let fragment = record::create_shader(&mut context, GL_FRAGMENT_SHADER);
    record::shader_source(&mut context, fragment, FS);
    record::compile_shader(&mut context, fragment);
    let program = record::create_program(&mut context);
    record::attach_shader(&mut context, program, fragment);

    // ES 3.0 §7.3: a non-compute program links only with both a vertex and a fragment shader. Reporting
    // GL_LINK_STATUS = 1 for one stage alone hands the caller a program that can never draw, and makes a
    // genuine failed link impossible to construct through the public API.
    assert!(!record::link_program(&mut context, program));
    assert_eq!(
        query::get_programiv(&context, program, GL_LINK_STATUS),
        GL_FALSE as i32
    );
    assert!(
        !query::program_info_log(&context, program).is_empty(),
        "a failed link must carry an actionable reason"
    );

    // Attaching the missing stage makes the same program link.
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    record::shader_source(&mut context, vertex, VS);
    record::compile_shader(&mut context, vertex);
    record::attach_shader(&mut context, program, vertex);
    assert!(record::link_program(&mut context, program));
}

// ---- name validity across the lifecycle ----------------------------------------------------------

#[test]
fn a_deleted_name_stops_being_recognized_and_a_never_generated_one_never_was() {
    let mut context = ctx();
    let texture = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    assert!(
        context.is_texture_name(texture),
        "a bound generated name is a texture object"
    );
    assert!(context.delete_texture(texture));
    assert!(
        !context.is_texture_name(texture),
        "glIsTexture must be GL_FALSE for a deleted name"
    );
    // ES 3.0 §6.1.5: glIs* is GL_FALSE for a name that was never generated, with no error raised.
    assert!(!context.is_texture_name(0xFFFF));
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
}
