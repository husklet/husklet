//! Query / introspection tests for the `gl*Get*` ops a real GLES app polls during init and every frame:
//! identity strings, capability limits, bound-object state, shader/program compile+link status, the
//! uniform/attribute reflection lookups, `glPixelStorei` round-trip, and the `glGetError` register.
//!
//! Driven directly against `hl_gl::service::query` + `record` (no socket, no GPU, no guest cdylib): these
//! are pure state inspections, so a plain `GlContext` is the whole fixture.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{query, record};

const VS: &str = "attribute vec2 aPos;\nattribute vec3 aColor;\nvarying vec3 vColor;\nvoid main(){ vColor = aColor; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
// A fragment shader with one data uniform (vec4) AND one sampler uniform (sampler2D).
const FS: &str = "precision mediump float;\nvarying vec3 vColor;\nuniform vec4 uTint;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vColor.xy) * uTint; }\n";

fn ctx_800x600() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 800, height: 600 };
    c
}

/// Compile+link the `VS`/`FS` pair and return the linked program name (bound as current).
fn linked_program(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

fn as_str(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap()
}

// ---- identity strings ----------------------------------------------------------------------------

#[test]
fn get_string_advertises_gles3_identity() {
    assert_eq!(as_str(query::gl_string(GL_VERSION)), "OpenGL ES 3.0 hl-gl");
    assert!(as_str(query::gl_string(GL_VERSION)).contains("OpenGL ES 3.0"));
    assert_eq!(as_str(query::gl_string(GL_SHADING_LANGUAGE_VERSION)), "OpenGL ES GLSL ES 3.00");
    // Vendor / renderer are non-empty and NUL-terminated.
    assert!(!as_str(query::gl_string(GL_VENDOR)).is_empty());
    assert!(!as_str(query::gl_string(GL_RENDERER)).is_empty());
    // An unknown name yields the empty string, never a null pointer.
    assert_eq!(as_str(query::gl_string(0xDEAD)), "");
}

#[test]
fn num_extensions_matches_the_extension_string() {
    // GL_NUM_EXTENSIONS must agree with the (empty) GL_EXTENSIONS list so an ES3 enumerator never walks
    // off the end.
    let c = ctx_800x600();
    let mut buf = [0i32; 4];
    assert_eq!(query::get_integerv(&c, GL_NUM_EXTENSIONS, &mut buf), 1);
    assert_eq!(buf[0], 0);
    assert_eq!(as_str(query::gl_string(GL_EXTENSIONS)), "");
}

// ---- glGetIntegerv -------------------------------------------------------------------------------

#[test]
fn get_integerv_limits_are_positive_and_sane() {
    let c = ctx_800x600();
    let mut b = [0i32; 4];

    assert_eq!(query::get_integerv(&c, GL_MAX_TEXTURE_SIZE, &mut b), 1);
    assert!(b[0] > 0, "GL_MAX_TEXTURE_SIZE must be > 0, got {}", b[0]);

    query::get_integerv(&c, GL_MAX_VERTEX_ATTRIBS, &mut b);
    assert_eq!(b[0], 16);

    query::get_integerv(&c, GL_MAX_TEXTURE_IMAGE_UNITS, &mut b);
    assert_eq!(b[0], 8);

    query::get_integerv(&c, GL_MAJOR_VERSION, &mut b);
    assert_eq!(b[0], 3);
    query::get_integerv(&c, GL_MINOR_VERSION, &mut b);
    assert_eq!(b[0], 0);

    // GL_MAX_VIEWPORT_DIMS returns 2 values.
    assert_eq!(query::get_integerv(&c, GL_MAX_VIEWPORT_DIMS, &mut b), 2);
    assert!(b[0] > 0 && b[1] > 0);

    // An unrecognized pname writes a single 0 (benign fallback).
    assert_eq!(query::get_integerv(&c, 0xBEEF, &mut b), 1);
    assert_eq!(b[0], 0);
}

#[test]
fn get_integerv_reads_live_bindings_and_viewport() {
    let mut c = ctx_800x600();
    let mut b = [0i32; 4];

    // Fresh context: the viewport reports the surface size (GL initializes it to the surface).
    assert_eq!(query::get_integerv(&c, GL_VIEWPORT, &mut b), 4);
    assert_eq!([b[0], b[1], b[2], b[3]], [0, 0, 800, 600]);

    // After glViewport, the stored rect is reported verbatim.
    record::viewport(&mut c, [10, 20, 320, 240]);
    query::get_integerv(&c, GL_VIEWPORT, &mut b);
    assert_eq!([b[0], b[1], b[2], b[3]], [10, 20, 320, 240]);

    // Bound array buffer + current program surface through the integer queries.
    let vbo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    query::get_integerv(&c, GL_ARRAY_BUFFER_BINDING, &mut b);
    assert_eq!(b[0], vbo as i32);

    let prog = linked_program(&mut c);
    query::get_integerv(&c, GL_CURRENT_PROGRAM, &mut b);
    assert_eq!(b[0], prog as i32);

    // Active texture unit tracks glActiveTexture.
    record::active_texture(&mut c, GL_TEXTURE0 + 3);
    query::get_integerv(&c, GL_ACTIVE_TEXTURE, &mut b);
    assert_eq!(b[0] as u32, GL_TEXTURE0 + 3);
}

// ---- glGetFloatv / glGetBooleanv -----------------------------------------------------------------

#[test]
fn get_floatv_and_booleanv_read_state() {
    let mut c = ctx_800x600();
    record::clear_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    let mut f = [0f32; 4];
    assert_eq!(query::get_floatv(&c, GL_COLOR_CLEAR_VALUE, &mut f), 4);
    assert_eq!(f, [0.25, 0.5, 0.75, 1.0]);

    let mut bl = [0u8; 4];
    query::get_booleanv(&c, GL_BLEND, &mut bl);
    assert_eq!(bl[0], 0);
    record::enable(&mut c, GL_BLEND);
    query::get_booleanv(&c, GL_BLEND, &mut bl);
    assert_eq!(bl[0], 1);
}

// ---- glPixelStorei -------------------------------------------------------------------------------

#[test]
fn pixel_store_round_trips_and_rejects_bad_values() {
    let mut c = ctx_800x600();
    let mut b = [0i32; 4];

    // Default alignment is GL's documented 4.
    query::get_integerv(&c, GL_UNPACK_ALIGNMENT, &mut b);
    assert_eq!(b[0], 4);

    // A valid alignment round-trips.
    record::pixel_store(&mut c, GL_UNPACK_ALIGNMENT, 1);
    query::get_integerv(&c, GL_UNPACK_ALIGNMENT, &mut b);
    assert_eq!(b[0], 1);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // An invalid alignment raises GL_INVALID_VALUE and leaves the value unchanged.
    record::pixel_store(&mut c, GL_UNPACK_ALIGNMENT, 3);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    query::get_integerv(&c, GL_UNPACK_ALIGNMENT, &mut b);
    assert_eq!(b[0], 1);
}

// ---- glGetShaderiv / glGetProgramiv --------------------------------------------------------------

#[test]
fn shader_and_program_status_reflect_compile_and_link() {
    let mut c = ctx_800x600();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);

    assert_eq!(query::get_shaderiv(&c, vs, GL_COMPILE_STATUS), GL_TRUE as i32);
    assert_eq!(query::get_shaderiv(&c, vs, GL_INFO_LOG_LENGTH), 0);
    assert_eq!(query::get_shaderiv(&c, vs, GL_SHADER_TYPE), GL_VERTEX_SHADER as i32);
    assert_eq!(query::get_shaderiv(&c, vs, GL_SHADER_SOURCE_LENGTH), VS.len() as i32 + 1);
    // An unknown shader name reports 0.
    assert_eq!(query::get_shaderiv(&c, 9999, GL_COMPILE_STATUS), 0);

    let prog = linked_program(&mut c);
    assert_eq!(query::get_programiv(&c, prog, GL_LINK_STATUS), GL_TRUE as i32);
    assert_eq!(query::get_programiv(&c, prog, GL_INFO_LOG_LENGTH), 0);
    assert_eq!(query::get_programiv(&c, prog, GL_ATTACHED_SHADERS), 2);
    // Two active attributes (aPos, aColor) and two active uniforms (uTint data + uTex sampler).
    assert_eq!(query::get_programiv(&c, prog, GL_ACTIVE_ATTRIBUTES), 2);
    assert_eq!(query::get_programiv(&c, prog, GL_ACTIVE_UNIFORMS), 2);
}

#[test]
fn info_logs_are_empty() {
    // The lib layer reports length 0 for a clean compile/link; the shim writes an empty NUL-terminated
    // string. Here we assert the modeled INFO_LOG_LENGTH the shim marshals from.
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    assert_eq!(query::get_shaderiv(&c, vs, GL_INFO_LOG_LENGTH), 0);
    assert_eq!(query::get_programiv(&c, prog, GL_INFO_LOG_LENGTH), 0);
}

// ---- glGetUniformLocation / glGetAttribLocation --------------------------------------------------

#[test]
fn uniform_location_resolves_declared_uniforms_and_minus_one_otherwise() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // The declared data uniform resolves to a valid (non-negative) location.
    let tint = query::uniform_location(&c, prog, "uTint");
    assert!(tint >= 0, "uTint must resolve, got {tint}");
    // The declared sampler uniform also resolves.
    let tex = query::uniform_location(&c, prog, "uTex");
    assert!(tex >= 0, "uTex must resolve, got {tex}");

    // An undeclared uniform is -1.
    assert_eq!(query::uniform_location(&c, prog, "uMissing"), -1);
    // An unknown program is -1.
    assert_eq!(query::uniform_location(&c, 4242, "uTint"), -1);
}

#[test]
fn attrib_location_matches_declaration_order() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // Declaration order in VS: aPos (0), aColor (1).
    assert_eq!(query::attrib_location(&c, prog, "aPos"), 0);
    assert_eq!(query::attrib_location(&c, prog, "aColor"), 1);
    // Unknown attribute / program → -1.
    assert_eq!(query::attrib_location(&c, prog, "aNope"), -1);
    assert_eq!(query::attrib_location(&c, 4242, "aPos"), -1);
}

// ---- glGetError ----------------------------------------------------------------------------------

#[test]
fn gl_error_round_trips_and_is_first_error_wins() {
    let mut c = ctx_800x600();
    // Nothing set yet.
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    c.set_gl_error(GL_INVALID_ENUM);
    // First-error-wins: a later error does not clobber the pending one.
    c.set_gl_error(GL_INVALID_VALUE);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    // Reading clears it.
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}
