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

/// The advertised inventory, in order. Adding an extension is a deliberate edit to THIS list, and every
/// other assertion below is derived from it — the string form, the indexed enumeration and the count are
/// three views of one inventory and must not be transcribed separately.
const ADVERTISED: &[&str] = &[
    "GL_KHR_debug",
    "GL_EXT_texture_format_BGRA8888",
    "GL_EXT_read_format_bgra",
    "GL_ANGLE_robust_client_memory",
    "GL_CHROMIUM_bind_generates_resource",
    "GL_CHROMIUM_copy_texture",
    "GL_ANGLE_client_arrays",
    "GL_ANGLE_webgl_compatibility",
    "GL_ANGLE_request_extension",
    "GL_OES_EGL_image",
    "GL_OES_EGL_sync",
    "GL_OES_rgb8_rgba8",
    "GL_OES_depth24",
    "GL_OES_mapbuffer",
    "GL_EXT_color_buffer_float",
    "GL_OES_texture_npot",
];

/// The three ways an application can ask what is advertised must give the same answer.
///
/// The driver keeps the inventory twice — a space-separated string for `glGetString` and an array for
/// `glGetStringi` — and nothing tied them together. That is the same drift shape as two copies of a
/// packing rule: an extension added to one and not the other is advertised to whichever query the
/// application happens to use, which is the worst possible half-promise. The string is now DERIVED from
/// the array here, so a one-sided edit fails rather than shipping.
#[test]
fn num_extensions_matches_the_extension_string() {
    let c = ctx_800x600();
    let mut buf = [0i32; 4];

    assert_eq!(query::get_integerv(&c, GL_NUM_EXTENSIONS, &mut buf), 1);
    assert_eq!(
        buf[0] as usize,
        ADVERTISED.len(),
        "GL_NUM_EXTENSIONS counts the inventory"
    );
    assert_eq!(
        as_str(query::gl_string(GL_EXTENSIONS)),
        ADVERTISED.join(" "),
        "the glGetString form is the indexed inventory joined by spaces"
    );
    for (index, expected) in ADVERTISED.iter().enumerate() {
        assert_eq!(
            as_str(query::string_i(GL_EXTENSIONS, index as u32).unwrap()),
            *expected,
            "glGetStringi({index})"
        );
    }
    assert!(
        query::string_i(GL_EXTENSIONS, ADVERTISED.len() as u32).is_none(),
        "one past the end is out of range"
    );

    query::get_integerv(&c, GL_NUM_REQUESTABLE_EXTENSIONS_ANGLE, &mut buf);
    assert_eq!(buf[0], 0);
    assert!(query::string_i(GL_REQUESTABLE_EXTENSIONS_ANGLE, 0).is_none());
    assert!(
        ADVERTISED.contains(&"GL_OES_texture_npot"),
        "full NPOT repeat/mipmap behavior is advertised so ES2 applications apply the matching rules"
    );
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

    // The GLES3 minimum per stage (16), backed by the translator's link-time sampler check.
    query::get_integerv(&c, GL_MAX_TEXTURE_IMAGE_UNITS, &mut b);
    assert_eq!(b[0], 16);
    query::get_integerv(&c, GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS, &mut b);
    assert_eq!(
        b[0], 32,
        "both stages together, the modelled texture-unit bank"
    );

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
    // consistent with the GPU-exec backend: BOTH real executors advertise `max_texture_2d == 8192` and
    // `hl_gpu`'s runtime validation rejects a larger texture, so 8192 is the honest ceiling.
    let c = ctx_800x600();
    let mut b = [0i32; 4];

    // GL_MAX_TEXTURE_SIZE (and the cube-map / renderbuffer edges that share it) are the executor ceiling
    // — and never the -455764240-style garbage of an untouched out-param.
    for pname in [
        GL_MAX_TEXTURE_SIZE,
        GL_MAX_CUBE_MAP_TEXTURE_SIZE,
        GL_MAX_RENDERBUFFER_SIZE,
    ] {
        assert_eq!(query::get_integerv(&c, pname, &mut b), 1);
        assert_eq!(
            b[0], 8192,
            "limit {pname:#x} must be the 8192 executor ceiling"
        );
    }

    // GL_MAX_VIEWPORT_DIMS reports two positive dims consistent with the texture ceiling.
    assert_eq!(query::get_integerv(&c, GL_MAX_VIEWPORT_DIMS, &mut b), 2);
    assert_eq!([b[0], b[1]], [8192, 8192]);

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
fn polygon_offset_state_round_trips_and_invalid_caps_are_rejected() {
    let mut c = ctx_800x600();
    let mut floats = [0.0f32; 4];
    let mut integers = [0i32; 4];
    let mut booleans = [0u8; 4];

    assert!(!c.is_enabled(GL_POLYGON_OFFSET_FILL));
    record::polygon_offset(&mut c, 2.25, -3.75);
    assert_eq!(query::get_floatv(&c, GL_POLYGON_OFFSET_FACTOR, &mut floats), 1);
    assert_eq!(floats[0], 2.25);
    assert_eq!(query::get_floatv(&c, GL_POLYGON_OFFSET_UNITS, &mut floats), 1);
    assert_eq!(floats[0], -3.75);
    assert_eq!(query::get_integerv(&c, GL_POLYGON_OFFSET_FACTOR, &mut integers), 1);
    assert_eq!(integers[0], 2);
    assert_eq!(query::get_integerv(&c, GL_POLYGON_OFFSET_UNITS, &mut integers), 1);
    assert_eq!(integers[0], -4);
    assert_eq!(query::get_booleanv(&c, GL_POLYGON_OFFSET_FACTOR, &mut booleans), 1);
    assert_eq!(booleans[0], GL_TRUE as u8);

    c.enable(GL_POLYGON_OFFSET_FILL);
    assert!(c.is_enabled(GL_POLYGON_OFFSET_FILL));
    c.disable(GL_POLYGON_OFFSET_FILL);
    assert!(!c.is_enabled(GL_POLYGON_OFFSET_FILL));

    c.enable(u32::MAX);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert!(!c.is_enabled(GL_TRIANGLES));
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

#[test]
fn single_sample_limits_and_multisample_enables_report_truthful_state() {
    let mut c = ctx_800x600();
    let mut integers = [0i32; 4];
    let mut floats = [0.0f32; 4];
    let mut booleans = [0u8; 4];

    for pname in [GL_SAMPLE_BUFFERS, GL_SAMPLES] {
        assert_eq!(query::get_integerv(&c, pname, &mut integers), 1);
        assert_eq!(integers[0], 0, "pname {pname:#x}");
    }

    for (capability, initial) in [
        (GL_DITHER, true),
        (GL_SAMPLE_ALPHA_TO_COVERAGE, false),
        (GL_SAMPLE_COVERAGE, false),
    ] {
        assert_eq!(c.is_enabled(capability), initial);
        assert_eq!(query::get_integerv(&c, capability, &mut integers), 1);
        assert_eq!(integers[0] != 0, initial);
        assert_eq!(query::get_floatv(&c, capability, &mut floats), 1);
        assert_eq!(floats[0] != 0.0, initial);
        assert_eq!(query::get_booleanv(&c, capability, &mut booleans), 1);
        assert_eq!(booleans[0] != GL_FALSE as u8, initial);

        c.enable(capability);
        assert!(c.is_enabled(capability));
        c.disable(capability);
        assert!(!c.is_enabled(capability));
    }
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

#[test]
fn sample_coverage_value_and_invert_default_clamp_and_convert() {
    let mut c = ctx_800x600();
    let mut floats = [0.0f32; 4];
    let mut integers = [0i32; 4];
    let mut booleans = [0u8; 4];

    assert_eq!(query::get_floatv(&c, GL_SAMPLE_COVERAGE_VALUE, &mut floats), 1);
    assert_eq!(floats[0], 1.0);
    assert_eq!(query::get_booleanv(&c, GL_SAMPLE_COVERAGE_INVERT, &mut booleans), 1);
    assert_eq!(booleans[0], GL_FALSE as u8);

    for (input, expected) in [
        (-1.5, 0.0),
        (0.45, 0.45),
        (1.45, 1.0),
        (f32::NEG_INFINITY, 0.0),
        (f32::INFINITY, 1.0),
        (f32::NAN, 0.0),
    ] {
        record::sample_coverage(&mut c, input, true);
        query::get_floatv(&c, GL_SAMPLE_COVERAGE_VALUE, &mut floats);
        assert_eq!(floats[0], expected, "input {input:?}");
        query::get_integerv(&c, GL_SAMPLE_COVERAGE_VALUE, &mut integers);
        assert_eq!(integers[0], expected.round() as i32, "input {input:?}");
        query::get_booleanv(&c, GL_SAMPLE_COVERAGE_VALUE, &mut booleans);
        assert_eq!(booleans[0] != GL_FALSE as u8, expected != 0.0, "input {input:?}");
        query::get_floatv(&c, GL_SAMPLE_COVERAGE_INVERT, &mut floats);
        assert_eq!(floats[0], 1.0);
        query::get_integerv(&c, GL_SAMPLE_COVERAGE_INVERT, &mut integers);
        assert_eq!(integers[0], 1);
        query::get_booleanv(&c, GL_SAMPLE_COVERAGE_INVERT, &mut booleans);
        assert_eq!(booleans[0], GL_TRUE as u8);
        assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    }

    record::sample_coverage(&mut c, 0.5, false);
    query::get_booleanv(&c, GL_SAMPLE_COVERAGE_INVERT, &mut booleans);
    assert_eq!(booleans[0], GL_FALSE as u8);
}

#[test]
fn integer_state_converts_through_float_and_boolean_queries_with_full_arity() {
    let mut c = ctx_800x600();
    c.active_texture(GL_TEXTURE0 + 3);
    record::viewport(&mut c, [0, 20, 320, 240]);

    let mut floats = [-1.0; 4];
    assert_eq!(query::get_floatv(&c, GL_ACTIVE_TEXTURE, &mut floats), 1);
    assert_eq!(floats[0], (GL_TEXTURE0 + 3) as f32);
    assert_eq!(query::get_floatv(&c, GL_VIEWPORT, &mut floats), 4);
    assert_eq!(floats, [0.0, 20.0, 320.0, 240.0]);

    let mut booleans = [9; 4];
    assert_eq!(query::get_booleanv(&c, GL_ACTIVE_TEXTURE, &mut booleans), 1);
    assert_eq!(booleans[0], GL_TRUE as u8);
    assert_eq!(query::get_booleanv(&c, GL_VIEWPORT, &mut booleans), 4);
    assert_eq!(
        booleans,
        [GL_FALSE as u8, GL_TRUE as u8, GL_TRUE as u8, GL_TRUE as u8]
    );

    // Capability limits use the same conversion path as mutable state, covering the CTS cases whose
    // GetIntegerv control already passed while GetFloatv/GetBooleanv returned zero.
    assert_eq!(query::get_floatv(&c, GL_MAX_TEXTURE_SIZE, &mut floats), 1);
    assert!(floats[0] > 0.0);
    assert_eq!(
        query::get_booleanv(&c, GL_MAX_TEXTURE_SIZE, &mut booleans),
        1
    );
    assert_eq!(booleans[0], GL_TRUE as u8);
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

#[test]
fn three_dimensional_unpack_state_round_trips() {
    let mut context = ctx_800x600();
    let mut value = [0; 4];
    for (pname, wanted) in [(GL_UNPACK_IMAGE_HEIGHT, 17), (GL_UNPACK_SKIP_IMAGES, 3)] {
        record::pixel_store(&mut context, pname, wanted);
        assert_eq!(context.take_gl_error(), GL_NO_ERROR);
        assert_eq!(query::get_integerv(&context, pname, &mut value), 1);
        assert_eq!(value[0], wanted);
    }
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
fn differently_shaped_aliased_attributes_use_distinct_host_locations() {
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

    assert!(record::link_program(&mut c, prog));
    assert_eq!(query::attrib_location(&c, prog, "aPos"), 2);
    assert_eq!(query::attrib_location(&c, prog, "aColor"), 2);
    let program = c.programs.program(prog).unwrap();
    assert_ne!(
        program.attrib_host_locations["aPos"],
        program.attrib_host_locations["aColor"]
    );
    let mut hosts = program.host_attr_locations(2);
    hosts.sort_unstable();
    hosts.dedup();
    assert_eq!(hosts.len(), 2);
}

#[test]
fn inactive_aliased_attribute_is_legal_and_not_reflected() {
    let mut c = ctx_800x600();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "attribute vec4 live; attribute vec4 inactive; void main(){ vec4 p=live; if(0 != 0) { p += inactive; } gl_Position=p; }",
    );
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "void main(){gl_FragColor=vec4(1.0);}");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    record::bind_attrib(&mut c, prog, 15, "live");
    record::bind_attrib(&mut c, prog, 15, "inactive");
    assert!(record::link_program(&mut c, prog));
    assert_eq!(query::attrib_location(&c, prog, "live"), 15);
    assert_eq!(query::attrib_location(&c, prog, "inactive"), -1);
}

#[test]
fn sixteen_equivalent_public_aliases_share_one_host_location() {
    let mut c = ctx_800x600();
    let declarations = (0..16)
        .map(|index| format!("attribute vec4 a{index};"))
        .collect::<String>();
    let sum = (0..16)
        .map(|index| format!("a{index}"))
        .collect::<Vec<_>>()
        .join("+");
    let vs_source = format!("{declarations} void main(){{gl_Position={sum};}}");
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, &vs_source);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "void main(){gl_FragColor=vec4(1.0);}");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    for index in 0..16 {
        record::bind_attrib(&mut c, prog, 15, &format!("a{index}"));
    }
    assert!(record::link_program(&mut c, prog));
    let program = c.programs.program(prog).unwrap();
    let unique = program
        .attrib_host_locations
        .values()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 1);
    assert!(unique.iter().all(|location| *location < 16));
}

#[test]
fn sixteen_pairs_of_conditional_aliases_fit_sixteen_host_locations() {
    let mut c = ctx_800x600();
    let declarations = (0..32)
        .map(|index| format!("attribute vec4 a{index};"))
        .collect::<String>();
    let uses = (0..16)
        .map(|index| {
            format!(
                "if (u != 0.0) p += a{index}; else p += a{};",
                index + 16
            )
        })
        .collect::<String>();
    let source = format!(
        "{declarations} uniform float u; void main(){{vec4 p=vec4(0.0);{uses}gl_Position=p;}}"
    );
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, &source);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "void main(){gl_FragColor=vec4(1.0);}");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    for index in 0..16 {
        record::bind_attrib(&mut c, prog, index, &format!("a{index}"));
        record::bind_attrib(&mut c, prog, index, &format!("a{}", index + 16));
    }
    assert!(record::link_program(&mut c, prog));
    let program = c.programs.program(prog).unwrap();
    let unique = program
        .attrib_host_locations
        .values()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 16);
    for index in 0..16 {
        assert_eq!(query::attrib_location(&c, prog, &format!("a{index}")), index);
        assert_eq!(
            query::attrib_location(&c, prog, &format!("a{}", index + 16)),
            index
        );
    }
}

#[test]
fn public_attribute_span_beyond_the_limit_still_fails_link() {
    let mut c = ctx_800x600();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, "attribute mat2 a; void main(){gl_Position=vec4(a[0], a[1]);}");
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "void main(){gl_FragColor=vec4(1.0);}");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    record::bind_attrib(&mut c, prog, 15, "a");
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

#[test]
fn deqp_qualification_order_valid_sources_keep_position_attribute_live() {
    let parameter_sets = [
        ("const in float x", "out float x", "inout float x"),
        (
            "const in lowp float x",
            "out mediump float x",
            "inout mediump float x",
        ),
        ("const lowp float x", "mediump float x", "mediump float x"),
    ];
    for (input, output, inout) in parameter_sets {
        let vertex = format!(
            "precision mediump float;\nattribute highp vec4 dEQP_Position;\nfloat foo0({input}){{return x+1.0;}}\nvoid foo1({output}){{x=1.0;}}\nfloat foo2({inout}){{return x+1.0;}}\nvoid main(){{float result;foo1(result);float x0=foo0(1.0);foo2(result);gl_Position=dEQP_Position+vec4(x0*0.0);}}"
        );
        let mut c = ctx_800x600();
        let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
        record::shader_source(&mut c, vs, &vertex);
        record::compile_shader(&mut c, vs);
        let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
        record::shader_source(&mut c, fs, "precision mediump float;void main(){gl_FragColor=vec4(1.0);}");
        record::compile_shader(&mut c, fs);
        let prog = record::create_program(&mut c);
        record::attach_shader(&mut c, prog, vs);
        record::attach_shader(&mut c, prog, fs);
        assert!(record::link_program(&mut c, prog));
        assert!(query::attrib_location(&c, prog, "dEQP_Position") >= 0);
    }

    for declaration in [
        "invariant varying lowp float x0;",
        "invariant varying float x0;",
        "varying lowp float x0;",
    ] {
        let vertex = format!(
            "precision mediump float;\nattribute highp vec4 dEQP_Position;\n{declaration}\nvoid main(){{x0=1.0;gl_Position=dEQP_Position;}}"
        );
        let fragment = format!(
            "precision mediump float;\n{declaration}\nvoid main(){{gl_FragColor=vec4(x0);}}"
        );
        let mut c = ctx_800x600();
        let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
        record::shader_source(&mut c, vs, &vertex);
        record::compile_shader(&mut c, vs);
        let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
        record::shader_source(&mut c, fs, &fragment);
        record::compile_shader(&mut c, fs);
        let prog = record::create_program(&mut c);
        record::attach_shader(&mut c, prog, vs);
        record::attach_shader(&mut c, prog, fs);
        assert!(record::link_program(&mut c, prog), "{declaration}");
        assert!(query::attrib_location(&c, prog, "dEQP_Position") >= 0);
    }
}

// ---- glGetStringi --------------------------------------------------------------------------------

/// `glGetStringi` agrees with `GL_NUM_EXTENSIONS` and refuses what it should.
///
/// The NAMES are asserted once, in `num_extensions_matches_the_extension_string`, against the single
/// `ADVERTISED` list. This used to transcribe all fourteen of them a second time, which is two more
/// places to forget when the inventory changes and no more coverage than the derivation gives.
#[test]
fn get_stringi_is_consistent_with_num_extensions() {
    assert_eq!(query::num_extensions(), query::EXTENSIONS.len() as i32);
    assert_eq!(query::num_extensions() as usize, ADVERTISED.len());
    for index in 0..query::num_extensions() as u32 {
        assert!(
            query::string_i(GL_EXTENSIONS, index).is_some(),
            "every index below the count resolves ({index})"
        );
    }
    assert!(
        query::string_i(GL_EXTENSIONS, query::num_extensions() as u32).is_none(),
        "one past the end is out of range"
    );
    assert!(
        query::string_i(GL_VERSION, 0).is_none(),
        "an indexed query of a non-indexed name is refused"
    );
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

/// Every location a matrix or array attribute occupies must report its own component count.
///
/// A `mat2` at location 1 occupies locations 1 AND 2, and each is a separate vertex input the pipeline
/// must supply. Reflection matched a location EXACTLY, so column 1 was unknown: a declared-but-disabled
/// matrix attribute got no constant vertex buffer for its later columns, the shader then declared a
/// location the pipeline never supplied, pipeline creation failed and the draw wedged the context. That
/// is what stops GTK4's GPU renderers rendering. Each location carries ONE COLUMN, so the count is the
/// matrix's ROW count — `mat4x2` is four locations of two components, not one of eight.
#[test]
fn every_location_a_matrix_attribute_spans_reports_its_column_components() {
    const FRAG: &str = "#version 300 es\nprecision mediump float;\nout vec4 c;\n\
                        void main(){ c = vec4(1.0); }\n";
    // (declaration, rows per location, locations spanned)
    for (ty, rows, span) in [
        ("mat2", 2, 2),
        ("mat3", 3, 3),
        ("mat4", 4, 4),
        ("mat4x2", 2, 4),
        ("mat2x4", 4, 2),
    ] {
        let source = format!(
            "#version 300 es\nlayout(location = 0) in vec2 position;\n\
             layout(location = 1) in {ty} pair;\n\
             void main(){{ gl_Position = vec4(position, 0.0, 1.0) + vec4(pair[0], 0.0, 0.0).xyzw * 0.0; }}\n"
        );
        let mut c = ctx_800x600();
        let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
        record::shader_source(&mut c, vs, &source);
        record::compile_shader(&mut c, vs);
        let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
        record::shader_source(&mut c, fs, FRAG);
        record::compile_shader(&mut c, fs);
        let prog = record::create_program(&mut c);
        record::attach_shader(&mut c, prog, vs);
        record::attach_shader(&mut c, prog, fs);
        assert!(record::link_program(&mut c, prog), "{ty} must link");

        let program = c.programs.program(prog).expect("linked program");
        // The single-location attribute is unchanged.
        assert_eq!(program.vertex_attr_components(0), Some(2), "{ty}: position");
        // EVERY location the matrix spans answers with one column's components.
        for column in 0..span {
            assert_eq!(
                program.vertex_attr_components(1 + column),
                Some(rows),
                "{ty}: location {} is column {column} and must supply {rows} components",
                1 + column
            );
        }
        // And nothing beyond its span is claimed.
        assert_eq!(
            program.vertex_attr_components(1 + span),
            None,
            "{ty} must not claim the location after its last column"
        );
    }
}

/// Every pixel-store parameter this driver honours must also READ BACK.
///
/// All six of row-length and the two skips, pack and unpack, were recorded and applied on the upload and
/// readback paths, but only the two alignments appeared in the integer query table — the rest reported 0
/// whatever the application had set. A toolkit that saves this state, draws, and restores it therefore
/// wrote 0 back over its own `glPixelStorei`, silently undoing itself. ES 3.0 §2.2.2 requires every state
/// value to read back through Get*; write-only state is worse than unimplemented state, because the
/// application cannot detect it.
#[test]
fn every_recorded_pixel_store_parameter_reads_back() {
    let mut c = ctx_800x600();
    let mut out = [0i32; 4];

    // Defaults first: GL initializes row length and both skips to 0, alignments to 4.
    for (pname, default) in [
        (GL_UNPACK_ALIGNMENT, 4),
        (GL_PACK_ALIGNMENT, 4),
        (GL_UNPACK_ROW_LENGTH, 0),
        (GL_UNPACK_SKIP_ROWS, 0),
        (GL_UNPACK_SKIP_PIXELS, 0),
        (GL_PACK_ROW_LENGTH, 0),
        (GL_PACK_SKIP_ROWS, 0),
        (GL_PACK_SKIP_PIXELS, 0),
    ] {
        out[0] = -1;
        assert_eq!(query::get_integerv(&c, pname, &mut out), 1, "{pname:#x}");
        assert_eq!(out[0], default, "{pname:#x} default");
    }

    // Then a distinct value per parameter, so a query reading the wrong field cannot pass.
    for (index, pname) in [
        GL_UNPACK_ROW_LENGTH,
        GL_UNPACK_SKIP_ROWS,
        GL_UNPACK_SKIP_PIXELS,
        GL_PACK_ROW_LENGTH,
        GL_PACK_SKIP_ROWS,
        GL_PACK_SKIP_PIXELS,
    ]
    .into_iter()
    .enumerate()
    {
        let value = 3 + index as i32;
        record::pixel_store(&mut c, pname, value);
        out[0] = -1;
        assert_eq!(query::get_integerv(&c, pname, &mut out), 1, "{pname:#x}");
        assert_eq!(out[0], value, "{pname:#x} must read back what was set");
    }

    // A rejected value leaves the parameter — and its readback — unchanged.
    record::pixel_store(&mut c, GL_UNPACK_ROW_LENGTH, -1);
    out[0] = -1;
    query::get_integerv(&c, GL_UNPACK_ROW_LENGTH, &mut out);
    assert_eq!(
        out[0], 3,
        "a refused glPixelStorei must not disturb the state"
    );
}

/// Blend state, the VAO binding, and the per-attribute array state must READ BACK.
///
/// All three families answered a constant `0` while the features themselves worked. That is the worst
/// shape a query defect can take: every embedded toolkit saves this state, draws, and restores what it
/// read — so a `0` is not an unknown, it is the value the application then installs. `GL_BLEND_SRC_RGB`
/// reading 0 makes the restore `glBlendFunc(GL_ZERO, GL_ZERO)`.
#[test]
fn blend_state_reads_back_what_blend_func_and_equation_set() {
    let mut c = ctx_800x600();
    let mut out = [0i32; 4];
    record::blend_func_separate(
        &mut c,
        GL_SRC_ALPHA,
        GL_ONE_MINUS_SRC_ALPHA,
        GL_ONE,
        GL_ZERO,
    );
    record::blend_equation_separate(&mut c, GL_FUNC_SUBTRACT, GL_MIN);

    for (pname, want) in [
        (GL_BLEND_SRC_RGB, GL_SRC_ALPHA),
        (GL_BLEND_DST_RGB, GL_ONE_MINUS_SRC_ALPHA),
        (GL_BLEND_SRC_ALPHA_STATE, GL_ONE),
        (GL_BLEND_DST_ALPHA, GL_ZERO),
        (GL_BLEND_EQUATION_RGB, GL_FUNC_SUBTRACT),
        (GL_BLEND_EQUATION_ALPHA, GL_MIN),
    ] {
        assert_eq!(query::get_integerv(&c, pname, &mut out), 1);
        assert_eq!(
            out[0], want as i32,
            "pname {pname:#x} must read back its enum"
        );
    }

    let mut colour = [0f32; 4];
    record::blend_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    assert_eq!(query::get_floatv(&c, GL_BLEND_COLOR, &mut colour), 4);
    assert_eq!(colour, [0.25, 0.5, 0.75, 1.0]);
}

#[test]
fn vertex_array_binding_and_attribute_array_state_read_back() {
    let mut c = ctx_800x600();
    let mut out = [0i32; 4];

    let vao = c.gen_vertex_array();
    c.bind_vertex_array(vao);
    assert_eq!(
        query::get_integerv(&c, GL_VERTEX_ARRAY_BINDING, &mut out),
        1
    );
    assert_eq!(out[0], vao as i32);

    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::vertex_attrib_pointer(&mut c, 1, 3, GL_SHORT, true, 12, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::vertex_attrib_divisor(&mut c, 1, 2);

    let get = |pname| query::get_vertex_attrib(&c, 1, pname);
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_ENABLED), Some(1));
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_SIZE), Some(3));
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_STRIDE), Some(12));
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_TYPE), Some(GL_SHORT as i32));
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_NORMALIZED), Some(1));
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING), Some(vbo as i32));
    assert_eq!(get(GL_VERTEX_ATTRIB_ARRAY_DIVISOR), Some(2));

    // Attribute state is VAO state: the default VAO's attribute 1 was never enabled, and the pair is what
    // distinguishes a real query from one that answers a constant.
    c.bind_vertex_array(0);
    assert_eq!(
        query::get_vertex_attrib(&c, 1, GL_VERTEX_ATTRIB_ARRAY_ENABLED),
        Some(0),
        "the default VAO has its own, untouched attribute state"
    );

    // An index past the attribute bank has no answer at all, so the caller raises GL_INVALID_VALUE.
    assert_eq!(
        query::get_vertex_attrib(&c, 9999, GL_VERTEX_ATTRIB_ARRAY_ENABLED),
        None
    );
}

/// `glCullFace` and `glFrontFace` must read back. Both were absent from the integer query table and fell
/// through to `0`, which is not even a legal enum for either — and the RENDERING was correct, so nothing
/// downstream flagged it. A toolkit that saves and restores this state installed `0` for the mode it had
/// set.
#[test]
fn cull_face_mode_and_front_face_read_back() {
    let mut c = ctx_800x600();
    let mut out = [0i32; 4];

    // The initial state (ES 3.0 table 6.9): GL_BACK and GL_CCW.
    assert_eq!(query::get_integerv(&c, GL_CULL_FACE_MODE, &mut out), 1);
    assert_eq!(out[0], GL_BACK as i32);
    assert_eq!(query::get_integerv(&c, GL_FRONT_FACE, &mut out), 1);
    assert_eq!(out[0], GL_CCW as i32);

    for mode in [GL_FRONT, GL_BACK, GL_FRONT_AND_BACK] {
        record::cull_face(&mut c, mode);
        query::get_integerv(&c, GL_CULL_FACE_MODE, &mut out);
        assert_eq!(out[0], mode as i32, "glCullFace({mode:#x}) must read back");
    }
    for winding in [GL_CW, GL_CCW] {
        record::front_face(&mut c, winding);
        query::get_integerv(&c, GL_FRONT_FACE, &mut out);
        assert_eq!(out[0], winding as i32);
    }
}

/// `GL_DEPTH_RANGE` reported a permanent `[0, 1]` beside a comment asserting `glDepthRangef` was a no-op.
/// It is not a no-op, and a state value that cannot read back is how the write side went unnoticed.
#[test]
fn the_depth_range_reads_back_through_both_getters() {
    let mut c = ctx_800x600();
    let mut floats = [0f32; 4];
    let mut ints = [0i32; 4];

    assert_eq!(query::get_floatv(&c, GL_DEPTH_RANGE, &mut floats), 2);
    assert_eq!(&floats[..2], &[0.0, 1.0], "the initial range");

    record::depth_range(&mut c, 0.25, 0.75);
    assert_eq!(query::get_floatv(&c, GL_DEPTH_RANGE, &mut floats), 2);
    assert_eq!(&floats[..2], &[0.25, 0.75]);
    assert_eq!(query::get_integerv(&c, GL_DEPTH_RANGE, &mut ints), 2);
    assert_eq!(&ints[..2], &[0, 0], "0.25 and 0.75 both truncate to 0");
}

#[test]
fn depth_hint_cube_and_stencil_state_read_back_live() {
    let mut c = ctx_800x600();
    let mut out = [0i32; 4];

    for (pname, expected) in [
        (GL_DEPTH_FUNC, GL_LESS as i32),
        (GL_GENERATE_MIPMAP_HINT, GL_DONT_CARE as i32),
        (GL_STENCIL_FUNC, GL_ALWAYS as i32),
        (GL_STENCIL_BACK_FUNC, GL_ALWAYS as i32),
        (GL_STENCIL_REF, 0),
        (GL_STENCIL_BACK_REF, 0),
        (GL_STENCIL_VALUE_MASK, -1),
        (GL_STENCIL_BACK_VALUE_MASK, -1),
        (GL_STENCIL_WRITEMASK, -1),
        (GL_STENCIL_BACK_WRITEMASK, -1),
        (GL_STENCIL_FAIL, GL_KEEP as i32),
        (GL_STENCIL_BACK_FAIL, GL_KEEP as i32),
    ] {
        assert_eq!(query::get_integerv(&c, pname, &mut out), 1);
        assert_eq!(out[0], expected, "default pname {pname:#x}");
    }

    record::depth_func(&mut c, GL_GREATER);
    record::hint(&mut c, GL_GENERATE_MIPMAP_HINT, GL_NICEST);
    let cube = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_CUBE_MAP, cube);
    record::stencil_func_separate(&mut c, GL_FRONT, GL_LESS, 3, 0x12);
    record::stencil_func_separate(&mut c, GL_BACK, GL_GREATER, 7, 0x34);
    record::stencil_op_separate(&mut c, GL_FRONT, GL_REPLACE, GL_INCR, GL_DECR);
    record::stencil_op_separate(&mut c, GL_BACK, GL_INVERT, GL_INCR_WRAP, GL_DECR_WRAP);
    record::stencil_mask_separate(&mut c, GL_FRONT, 0x56);
    record::stencil_mask_separate(&mut c, GL_BACK, 0x78);

    for (pname, expected) in [
        (GL_DEPTH_FUNC, GL_GREATER as i32),
        (GL_GENERATE_MIPMAP_HINT, GL_NICEST as i32),
        (GL_TEXTURE_BINDING_CUBE_MAP, cube as i32),
        (GL_STENCIL_FUNC, GL_LESS as i32),
        (GL_STENCIL_BACK_FUNC, GL_GREATER as i32),
        (GL_STENCIL_REF, 3),
        (GL_STENCIL_BACK_REF, 7),
        (GL_STENCIL_VALUE_MASK, 0x12),
        (GL_STENCIL_BACK_VALUE_MASK, 0x34),
        (GL_STENCIL_WRITEMASK, 0x56),
        (GL_STENCIL_BACK_WRITEMASK, 0x78),
        (GL_STENCIL_FAIL, GL_REPLACE as i32),
        (GL_STENCIL_BACK_FAIL, GL_INVERT as i32),
        (GL_STENCIL_PASS_DEPTH_FAIL, GL_INCR as i32),
        (GL_STENCIL_BACK_PASS_DEPTH_FAIL, GL_INCR_WRAP as i32),
        (GL_STENCIL_PASS_DEPTH_PASS, GL_DECR as i32),
        (GL_STENCIL_BACK_PASS_DEPTH_PASS, GL_DECR_WRAP as i32),
    ] {
        assert_eq!(query::get_integerv(&c, pname, &mut out), 1);
        assert_eq!(out[0], expected, "live pname {pname:#x}");
    }

    record::stencil_func(&mut c, GL_ALWAYS, 9, 0xaa);
    record::stencil_op(&mut c, GL_KEEP, GL_REPLACE, GL_INVERT);
    c.set_stencil_mask(0xbb);
    for (front, back, expected) in [
        (GL_STENCIL_FUNC, GL_STENCIL_BACK_FUNC, GL_ALWAYS as i32),
        (GL_STENCIL_REF, GL_STENCIL_BACK_REF, 9),
        (GL_STENCIL_VALUE_MASK, GL_STENCIL_BACK_VALUE_MASK, 0xaa),
        (GL_STENCIL_WRITEMASK, GL_STENCIL_BACK_WRITEMASK, 0xbb),
        (GL_STENCIL_FAIL, GL_STENCIL_BACK_FAIL, GL_KEEP as i32),
        (GL_STENCIL_PASS_DEPTH_FAIL, GL_STENCIL_BACK_PASS_DEPTH_FAIL, GL_REPLACE as i32),
        (GL_STENCIL_PASS_DEPTH_PASS, GL_STENCIL_BACK_PASS_DEPTH_PASS, GL_INVERT as i32),
    ] {
        for pname in [front, back] {
            query::get_integerv(&c, pname, &mut out);
            assert_eq!(out[0], expected, "joined pname {pname:#x}");
        }
    }
}

#[test]
fn unsigned_stencil_value_masks_convert_to_float_without_signed_narrowing() {
    let mut c = ctx_800x600();
    let mut out = [0.0f32; 4];

    for pname in [GL_STENCIL_VALUE_MASK, GL_STENCIL_BACK_VALUE_MASK] {
        assert_eq!(query::get_floatv(&c, pname, &mut out), 1);
        assert_eq!(out[0], u32::MAX as f32, "default pname {pname:#x}");
    }

    record::stencil_func_separate(&mut c, GL_FRONT, GL_ALWAYS, 0, 0x8000_0000);
    record::stencil_func_separate(&mut c, GL_BACK, GL_ALWAYS, 0, 0x7fff_ffff);
    for (pname, expected) in [
        (GL_STENCIL_VALUE_MASK, 0x8000_0000u32),
        (GL_STENCIL_BACK_VALUE_MASK, 0x7fff_ffffu32),
    ] {
        assert_eq!(query::get_floatv(&c, pname, &mut out), 1);
        assert_eq!(out[0], expected as f32, "live pname {pname:#x}");
    }
}
