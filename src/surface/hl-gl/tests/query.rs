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
    c.set_surface(GlSurface {
        have: true,
        width: 800,
        height: 600,
    });
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
    assert_eq!(as_str(query::gl_string(GL_VERSION)), "OpenGL ES 3.1 hl-gl");
    assert!(as_str(query::gl_string(GL_VERSION)).contains("OpenGL ES 3.1"));
    assert_eq!(
        as_str(query::gl_string(GL_SHADING_LANGUAGE_VERSION)),
        "OpenGL ES GLSL ES 3.10"
    );
    // Vendor / renderer are non-empty and NUL-terminated.
    assert!(!as_str(query::gl_string(GL_VENDOR)).is_empty());
    assert!(!as_str(query::gl_string(GL_RENDERER)).is_empty());
    // An unknown name yields the empty string, never a null pointer.
    assert_eq!(as_str(query::gl_string(0xDEAD)), "");
}

#[test]
fn num_extensions_matches_the_extension_string() {
    // The legacy string and indexed GLES3 extension inventory must agree.
    let c = ctx_800x600();
    let mut buf = [0i32; 4];
    assert_eq!(query::get_integerv(&c, GL_NUM_EXTENSIONS, &mut buf), 1);
    assert_eq!(buf[0], 11);
    assert_eq!(
        as_str(query::gl_string(GL_EXTENSIONS)),
        "GL_KHR_debug GL_EXT_texture_format_BGRA8888 GL_EXT_read_format_bgra GL_ANGLE_robust_client_memory GL_CHROMIUM_bind_generates_resource GL_CHROMIUM_copy_texture GL_ANGLE_client_arrays GL_ANGLE_webgl_compatibility GL_ANGLE_request_extension GL_OES_EGL_image GL_OES_EGL_sync"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 0).unwrap()),
        "GL_KHR_debug"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 1).unwrap()),
        "GL_EXT_texture_format_BGRA8888"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 2).unwrap()),
        "GL_EXT_read_format_bgra"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 3).unwrap()),
        "GL_ANGLE_robust_client_memory"
    );
    for (index, expected) in [
        (4, "GL_CHROMIUM_bind_generates_resource"),
        (5, "GL_CHROMIUM_copy_texture"),
        (6, "GL_ANGLE_client_arrays"),
        (7, "GL_ANGLE_webgl_compatibility"),
        (8, "GL_ANGLE_request_extension"),
        (9, "GL_OES_EGL_image"),
        (10, "GL_OES_EGL_sync"),
    ] {
        assert_eq!(
            as_str(query::string_i(GL_EXTENSIONS, index).unwrap()),
            expected
        );
    }
    assert!(query::string_i(GL_EXTENSIONS, 11).is_none());
    query::get_integerv(&c, GL_NUM_REQUESTABLE_EXTENSIONS_ANGLE, &mut buf);
    assert_eq!(buf[0], 0);
    assert!(query::string_i(GL_REQUESTABLE_EXTENSIONS_ANGLE, 0).is_none());
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
    assert_eq!(b[0], query::ES_MAJOR);
    query::get_integerv(&c, GL_MINOR_VERSION, &mut b);
    assert_eq!(b[0], query::ES_MINOR);

    // GL_MAX_VIEWPORT_DIMS returns 2 values.
    assert_eq!(query::get_integerv(&c, GL_MAX_VIEWPORT_DIMS, &mut b), 2);
    assert!(b[0] > 0 && b[1] > 0);

    // An unrecognized pname writes a single 0 (benign fallback).
    assert_eq!(query::get_integerv(&c, 0xBEEF, &mut b), 1);
    assert_eq!(b[0], 0);
}

#[test]
fn get_integerv_reports_truthful_executor_consistent_limits() {
    // The limits GTK/epoxy query during GL init must be TRUTHFUL (no garbage / uninitialized memory) and
    // consistent with the GPU-exec backend (`hl_gpu` Capabilities::full max_texture_2d = 16384).
    let c = ctx_800x600();
    let mut b = [0i32; 4];

    // GL_MAX_TEXTURE_SIZE (and the cube-map / renderbuffer edges that share it) are the executor ceiling,
    // not the old 4096 stand-in — and never the -455764240-style garbage of an untouched out-param.
    for pname in [
        GL_MAX_TEXTURE_SIZE,
        GL_MAX_CUBE_MAP_TEXTURE_SIZE,
        GL_MAX_RENDERBUFFER_SIZE,
    ] {
        assert_eq!(query::get_integerv(&c, pname, &mut b), 1);
        assert_eq!(
            b[0], 16384,
            "limit {pname:#x} must be the 16384 executor ceiling"
        );
    }

    // GL_MAX_VIEWPORT_DIMS reports two positive dims consistent with the texture ceiling.
    assert_eq!(query::get_integerv(&c, GL_MAX_VIEWPORT_DIMS, &mut b), 2);
    assert_eq!([b[0], b[1]], [16384, 16384]);

    // GLES3 MRT + batch limits GTK reads (previously fell through to the unknown-pname 0).
    query::get_integerv(&c, GL_MAX_COLOR_ATTACHMENTS, &mut b);
    assert_eq!(b[0], 4);
    query::get_integerv(&c, GL_MAX_DRAW_BUFFERS, &mut b);
    assert_eq!(b[0], 4);
    query::get_integerv(&c, GL_MAX_ELEMENTS_VERTICES, &mut b);
    assert!(
        b[0] >= 65536,
        "GL_MAX_ELEMENTS_VERTICES must be a large sane batch hint, got {}",
        b[0]
    );
    query::get_integerv(&c, GL_MAX_ELEMENTS_INDICES, &mut b);
    assert!(
        b[0] >= 65536,
        "GL_MAX_ELEMENTS_INDICES must be a large sane batch hint, got {}",
        b[0]
    );

    // Chromium rejects an ES3 context whose transform-feedback and uniform-buffer limits are below the
    // GLES3 minima. These match the indexed binding state actually modeled by GlContext.
    for (pname, minimum) in [
        (GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS, 4),
        (GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS, 64),
        (GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS, 4),
        (GL_MAX_VERTEX_UNIFORM_BLOCKS, 12),
        (GL_MAX_FRAGMENT_UNIFORM_BLOCKS, 12),
        (GL_MAX_COMBINED_UNIFORM_BLOCKS, 24),
        (GL_MAX_UNIFORM_BUFFER_BINDINGS, 24),
    ] {
        query::get_integerv(&c, pname, &mut b);
        assert!(
            b[0] >= minimum,
            "limit {pname:#x} must be at least {minimum}, got {}",
            b[0]
        );
    }
    query::get_integerv(&c, GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT, &mut b);
    assert_eq!(b[0], 256);
    for (pname, expected) in [
        (GL_MAX_3D_TEXTURE_SIZE, 2048),
        (GL_MAX_ARRAY_TEXTURE_LAYERS, 256),
        (GL_MAX_VERTEX_OUTPUT_COMPONENTS, 64),
        (GL_MAX_FRAGMENT_INPUT_COMPONENTS, 60),
        (GL_MIN_PROGRAM_TEXEL_OFFSET, -8),
        (GL_MAX_PROGRAM_TEXEL_OFFSET, 7),
    ] {
        query::get_integerv(&c, pname, &mut b);
        assert_eq!(b[0], expected, "unexpected limit for {pname:#x}");
    }

    // The other GLES3 program limits epoxy caches are all positive (never uninitialized).
    for pname in [
        GL_MAX_VERTEX_UNIFORM_VECTORS,
        GL_MAX_FRAGMENT_UNIFORM_VECTORS,
        GL_MAX_VARYING_VECTORS,
        GL_MAX_VERTEX_UNIFORM_COMPONENTS,
        GL_MAX_FRAGMENT_UNIFORM_COMPONENTS,
        GL_MAX_VARYING_COMPONENTS,
        GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS,
        GL_MAX_SAMPLES,
    ] {
        query::get_integerv(&c, pname, &mut b);
        assert!(b[0] > 0, "limit {pname:#x} must be > 0, got {}", b[0]);
    }

    let mut f = [0.0; 4];
    query::get_floatv(&c, GL_MAX_TEXTURE_LOD_BIAS, &mut f);
    assert!(f[0] >= 2.0);
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
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    query::get_integerv(&c, GL_ARRAY_BUFFER_BINDING, &mut b);
    assert_eq!(b[0], vbo as i32);

    let prog = linked_program(&mut c);
    query::get_integerv(&c, GL_CURRENT_PROGRAM, &mut b);
    assert_eq!(b[0], prog as i32);

    // Active texture unit tracks glActiveTexture.
    c.active_texture(GL_TEXTURE0 + 3);
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

#[test]
fn webgl_range_and_color_mask_queries_have_complete_arity() {
    let c = ctx_800x600();
    let mut floats = [0.0; 4];
    assert_eq!(
        query::get_floatv(&c, GL_ALIASED_POINT_SIZE_RANGE, &mut floats),
        2
    );
    assert_eq!(&floats[..2], &[1.0, 1.0]);

    let mut booleans = [0; 4];
    assert_eq!(
        query::get_booleanv(&c, GL_COLOR_WRITEMASK, &mut booleans),
        4
    );
    assert_eq!(booleans, [1, 1, 1, 1]);
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

    assert_eq!(
        query::get_shaderiv(&c, vs, GL_COMPILE_STATUS),
        GL_TRUE as i32
    );
    assert_eq!(query::get_shaderiv(&c, vs, GL_INFO_LOG_LENGTH), 0);
    assert_eq!(
        query::get_shaderiv(&c, vs, GL_SHADER_TYPE),
        GL_VERTEX_SHADER as i32
    );
    assert_eq!(
        query::get_shaderiv(&c, vs, GL_SHADER_SOURCE_LENGTH),
        VS.len() as i32 + 1
    );
    // An unknown shader name reports 0.
    assert_eq!(query::get_shaderiv(&c, 9999, GL_COMPILE_STATUS), 0);

    let prog = linked_program(&mut c);
    assert_eq!(
        query::get_programiv(&c, prog, GL_LINK_STATUS),
        GL_TRUE as i32
    );
    assert_eq!(query::get_programiv(&c, prog, GL_INFO_LOG_LENGTH), 0);
    assert_eq!(query::get_programiv(&c, prog, GL_ATTACHED_SHADERS), 2);
    // Two active attributes (aPos, aColor) and two active uniforms (uTint data + uTex sampler).
    assert_eq!(query::get_programiv(&c, prog, GL_ACTIVE_ATTRIBUTES), 2);
    assert_eq!(query::get_programiv(&c, prog, GL_ACTIVE_UNIFORMS), 2);
    assert_eq!(
        query::get_programiv(&c, prog, GL_ACTIVE_ATTRIBUTE_MAX_LENGTH),
        "aColor".len() as i32 + 1
    );
    assert_eq!(
        query::get_programiv(&c, prog, GL_ACTIVE_UNIFORM_MAX_LENGTH),
        "uTint".len().max("uTex".len()) as i32 + 1
    );
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

#[test]
fn attrib_location_honors_pre_link_name_binding() {
    let mut c = ctx_800x600();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);

    // Deliberately disagree with declaration order: aPos=0/aColor=1 in source.
    record::bind_attrib(&mut c, prog, 3, "aPos");
    record::bind_attrib(&mut c, prog, 1, "aColor");
    assert!(record::link_program(&mut c, prog));

    assert_eq!(query::attrib_location(&c, prog, "aPos"), 3);
    assert_eq!(query::attrib_location(&c, prog, "aColor"), 1);
    assert_eq!(
        c.programs
            .program(prog)
            .and_then(|program| program.vertex_attr_components(3)),
        Some(2)
    );
}

#[test]
fn conflicting_attribute_bindings_fail_link() {
    let mut c = ctx_800x600();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    record::bind_attrib(&mut c, prog, 2, "aPos");
    record::bind_attrib(&mut c, prog, 2, "aColor");

    assert!(!record::link_program(&mut c, prog));
    assert!(query::program_info_log(&c, prog).contains("attribute"));
}

// ---- glGetActiveUniform / glGetActiveAttrib ------------------------------------------------------

#[test]
fn active_uniform_reflects_declared_name_and_type() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // The GL_ACTIVE_UNIFORMS enumeration is data uniforms first (uTint: vec4), then samplers (uTex:
    // sampler2D) — the same order and count glGetProgramiv(GL_ACTIVE_UNIFORMS) reports (2).
    assert_eq!(query::get_programiv(&c, prog, GL_ACTIVE_UNIFORMS), 2);
    let tint = query::active_uniform(&c, prog, 0).expect("uniform 0");
    assert_eq!(tint.name, "uTint");
    assert_eq!(tint.gl_type, GL_FLOAT_VEC4);
    assert_eq!(tint.size, 1);

    let tex = query::active_uniform(&c, prog, 1).expect("uniform 1");
    assert_eq!(tex.name, "uTex");
    assert_eq!(tex.gl_type, GL_SAMPLER_2D);

    // An out-of-range index / unknown program reflects nothing.
    assert!(query::active_uniform(&c, prog, 2).is_none());
    assert!(query::active_uniform(&c, 4242, 0).is_none());
}

#[test]
fn active_attrib_reflects_declared_name_and_type() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // Declaration order in VS: aPos (vec2, index 0), aColor (vec3, index 1).
    let a0 = query::active_attrib(&c, prog, 0).expect("attrib 0");
    assert_eq!(a0.name, "aPos");
    assert_eq!(a0.gl_type, GL_FLOAT_VEC2);
    let a1 = query::active_attrib(&c, prog, 1).expect("attrib 1");
    assert_eq!(a1.name, "aColor");
    assert_eq!(a1.gl_type, GL_FLOAT_VEC3);

    assert!(query::active_attrib(&c, prog, 2).is_none());
}

// ---- glGetStringi --------------------------------------------------------------------------------

#[test]
fn get_stringi_is_consistent_with_num_extensions() {
    assert_eq!(query::num_extensions(), query::EXTENSIONS.len() as i32);
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 0).unwrap()),
        "GL_KHR_debug"
    );
    // A non-GL_EXTENSIONS name and an out-of-range extension index are both rejected.
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 1).unwrap()),
        "GL_EXT_texture_format_BGRA8888"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 2).unwrap()),
        "GL_EXT_read_format_bgra"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 3).unwrap()),
        "GL_ANGLE_robust_client_memory"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 4).unwrap()),
        "GL_CHROMIUM_bind_generates_resource"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 5).unwrap()),
        "GL_CHROMIUM_copy_texture"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 6).unwrap()),
        "GL_ANGLE_client_arrays"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 7).unwrap()),
        "GL_ANGLE_webgl_compatibility"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 8).unwrap()),
        "GL_ANGLE_request_extension"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 9).unwrap()),
        "GL_OES_EGL_image"
    );
    assert_eq!(
        as_str(query::string_i(GL_EXTENSIONS, 10).unwrap()),
        "GL_OES_EGL_sync"
    );
    assert!(query::string_i(GL_EXTENSIONS, 11).is_none());
    assert!(query::string_i(GL_VERSION, 0).is_none());
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
