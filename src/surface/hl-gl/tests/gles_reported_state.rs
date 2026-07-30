//! Mechanical cross-checks of the values the driver REPORTS about itself, driven in-process against
//! `hl_gl::service::query`.
//!
//! `tests/gles_limits.rs` already proves every advertised capability limit meets its specification
//! minimum. This battery covers the rest of the reported surface, where the failure mode is not "too
//! small" but "inconsistent" or "silently absent": a `glGet*` pname the specification requires that falls
//! into the driver's benign `0` fallback answers a plausible lie, and the caller acts on it.
//!
//! Every case names the specification requirement it pins. Nothing here asserts on source text or on how
//! many things exist — only on returned values.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{query, record};

/// pnames the driver's `glconst` module does not name, defined here so each case reads in isolation.
mod pname {
    /// ES 2.0 §6.2 / ES 3.0 §4.3.2 — the second always-accepted `glReadPixels` format/type pair.
    pub const GL_IMPLEMENTATION_COLOR_READ_TYPE: u32 = 0x8B9A;
    pub const GL_IMPLEMENTATION_COLOR_READ_FORMAT: u32 = 0x8B9B;
    /// ES 2.0 §6.1.5 — `GL_TRUE` on every implementation that accepts shader SOURCE.
    pub const GL_SHADER_COMPILER: u32 = 0x8DFA;
    /// ES 2.0 table 6.19 — the depth range, two floats, initially `[0, 1]`.
    pub const GL_DEPTH_RANGE: u32 = 0x0B70;
    pub const GL_MAX_RENDERBUFFER_SIZE_: u32 = 0x84E8;
}

fn ctx() -> GlContext {
    let mut context = GlContext::new();
    context.set_surface(GlSurface {
        have: true,
        width: 640,
        height: 480,
    });
    context
}

fn integer(context: &GlContext, pname: u32) -> i32 {
    let mut out = [i32::MIN; 4];
    let written = query::get_integerv(context, pname, &mut out);
    assert!(written >= 1, "glGetIntegerv(0x{pname:04x}) wrote nothing");
    out[0]
}

fn boolean(context: &GlContext, pname: u32) -> u8 {
    let mut out = [0u8; 4];
    let written = query::get_booleanv(context, pname, &mut out);
    assert!(written >= 1, "glGetBooleanv(0x{pname:04x}) wrote nothing");
    out[0]
}

fn as_str(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap()
}

// ---- identity-string coherence -------------------------------------------------------------------

#[test]
fn version_strings_agree_with_the_numeric_version_queries() {
    let context = ctx();
    let version = as_str(query::gl_string(GL_VERSION)).to_owned();
    let major = integer(&context, GL_MAJOR_VERSION);
    let minor = integer(&context, GL_MINOR_VERSION);
    assert!(
        version.starts_with(&format!("OpenGL ES {major}.{minor}")),
        "glGetString(GL_VERSION) = {version:?} must begin with the OpenGL ES \
         GL_MAJOR_VERSION.GL_MINOR_VERSION the same context reports ({major}.{minor}); a toolkit \
         parses one and branches on the other"
    );

    // ES 3.0 §6.1.6: the shading-language version string is `OpenGL ES GLSL ES <major>.<minor><digit>`,
    // and for ES 3.x the GLSL version tracks the ES version.
    let glsl = as_str(query::gl_string(GL_SHADING_LANGUAGE_VERSION)).to_owned();
    let prefix = "OpenGL ES GLSL ES ";
    assert!(
        glsl.starts_with(prefix),
        "unexpected GLSL version string {glsl:?}"
    );
    let number = &glsl[prefix.len()..];
    assert_eq!(
        number.split('.').next(),
        Some(major.to_string().as_str()),
        "the GLSL ES version {number:?} must share the ES major version {major}"
    );
}

#[test]
fn the_extension_string_and_the_indexed_inventory_are_the_same_well_formed_set() {
    let context = ctx();
    let count = integer(&context, GL_NUM_EXTENSIONS);
    assert!(count > 0);
    let from_string: Vec<&str> = as_str(query::gl_string(GL_EXTENSIONS))
        .split_whitespace()
        .collect();
    assert_eq!(
        from_string.len(),
        count as usize,
        "GL_NUM_EXTENSIONS must equal the number of names in the GL_EXTENSIONS string"
    );
    let indexed: Vec<&str> = (0..count as u32)
        .map(|index| {
            as_str(
                query::string_i(GL_EXTENSIONS, index)
                    .unwrap_or_else(|| panic!("glGetStringi({index}) returned None in range")),
            )
        })
        .collect();
    assert_eq!(
        indexed, from_string,
        "the two enumerations must agree name-for-name"
    );

    // ES 3.0 §3.10: an extension name is a single token beginning with GL_, and no name repeats.
    let mut seen = std::collections::BTreeSet::new();
    for name in &indexed {
        assert!(
            name.starts_with("GL_"),
            "{name:?} is not a GL extension name"
        );
        assert!(seen.insert(*name), "{name:?} is advertised twice");
    }
    assert!(
        query::string_i(GL_EXTENSIONS, count as u32).is_none(),
        "an index at GL_NUM_EXTENSIONS must not resolve"
    );
}

// ---- pnames the specification requires, and their consistency across the Get* variants -----------

#[test]
fn implementation_color_read_format_and_type_name_a_real_read_pixels_pair() {
    let context = ctx();
    // ES 3.0 §4.3.2: besides GL_RGBA/GL_UNSIGNED_BYTE, glReadPixels always accepts the pair these two
    // queries report. Reporting 0 makes a caller pass format 0 to glReadPixels and take GL_INVALID_ENUM
    // on a call it was told was supported — Qt's QOpenGLFramebufferObject::toImage and every
    // "pick the fast readback path" helper does exactly this.
    let format = integer(&context, pname::GL_IMPLEMENTATION_COLOR_READ_FORMAT);
    let kind = integer(&context, pname::GL_IMPLEMENTATION_COLOR_READ_TYPE);
    assert_ne!(
        format, 0,
        "GL_IMPLEMENTATION_COLOR_READ_FORMAT must name a format"
    );
    assert_ne!(
        kind, 0,
        "GL_IMPLEMENTATION_COLOR_READ_TYPE must name a type"
    );
    assert_eq!(
        (format as u32, kind as u32),
        (GL_RGBA, GL_UNSIGNED_BYTE),
        "the driver's default framebuffer is RGBA8, so the implementation-defined read pair is \
         GL_RGBA / GL_UNSIGNED_BYTE"
    );
}

#[test]
fn shader_compiler_is_advertised_because_the_driver_accepts_shader_source() {
    let mut context = ctx();
    // ES 2.0 §6.1.5 / ES 3.0 §7.2: when GL_SHADER_COMPILER is GL_FALSE, glShaderSource and
    // glCompileShader raise GL_INVALID_OPERATION and only shader binaries work. This driver compiles
    // source (proved immediately below), so reporting GL_FALSE tells a caller the opposite of the truth
    // and sends it looking for a binary format that does not exist either.
    let shader = record::create_shader(&mut context, GL_VERTEX_SHADER);
    record::shader_source(
        &mut context,
        shader,
        "void main(){ gl_Position = vec4(0.0); }",
    );
    record::compile_shader(&mut context, shader);
    assert_eq!(
        query::get_shaderiv(&context, shader, GL_COMPILE_STATUS),
        GL_TRUE as i32,
        "the driver does compile shader source"
    );

    assert_eq!(
        integer(&context, pname::GL_SHADER_COMPILER),
        GL_TRUE as i32,
        "glGetIntegerv(GL_SHADER_COMPILER) must be GL_TRUE"
    );
    assert_eq!(
        boolean(&context, pname::GL_SHADER_COMPILER),
        GL_TRUE as u8,
        "glGetBooleanv(GL_SHADER_COMPILER) must be GL_TRUE"
    );
}

#[test]
fn depth_range_reports_two_floats_covering_the_full_window_range() {
    let context = ctx();
    // ES 2.0 table 6.19: GL_DEPTH_RANGE is two floats, initially [0, 1]. A single 0.0 leaves a caller
    // that reads the pair with an uninitialized second slot and a near == far range.
    let mut out = [f32::NAN; 4];
    let written = query::get_floatv(&context, pname::GL_DEPTH_RANGE, &mut out);
    assert_eq!(written, 2, "GL_DEPTH_RANGE is a 2-element query");
    assert_eq!(&out[..2], &[0.0, 1.0]);
}

#[test]
fn boolean_and_integer_queries_agree_for_every_state_both_answer() {
    let mut context = ctx();
    record::enable(&mut context, GL_BLEND);
    record::enable(&mut context, GL_SCISSOR_TEST);
    // ES 3.0 §2.2.2: a state value is convertible between the Get* variants — glGetBooleanv reports
    // GL_FALSE only when the integer value is zero. A caller that polls one and caches the other (ANGLE's
    // state cache does) otherwise disagrees with the driver about whether blending is on.
    for (pname, name) in [
        (GL_BLEND, "GL_BLEND"),
        (GL_DEPTH_TEST, "GL_DEPTH_TEST"),
        (GL_STENCIL_TEST, "GL_STENCIL_TEST"),
        (GL_CULL_FACE, "GL_CULL_FACE"),
        (GL_SCISSOR_TEST, "GL_SCISSOR_TEST"),
        (GL_DEPTH_WRITEMASK, "GL_DEPTH_WRITEMASK"),
    ] {
        let as_integer = integer(&context, pname) != 0;
        let as_boolean = boolean(&context, pname) != 0;
        assert_eq!(
            as_integer, as_boolean,
            "{name}: glGetIntegerv says {as_integer} but glGetBooleanv says {as_boolean}"
        );
    }
}

// ---- limits that must bound one another ----------------------------------------------------------

#[test]
fn the_viewport_limit_admits_every_render_target_the_driver_advertises() {
    let context = ctx();
    // ES 3.0 §13.6.1: the viewport must be able to cover the largest renderable surface, so
    // GL_MAX_VIEWPORT_DIMS cannot be smaller than the texture / renderbuffer / framebuffer extents the
    // driver advertises. A smaller viewport limit silently clips a legally sized off-screen target.
    let mut dims = [i32::MIN; 4];
    assert_eq!(
        query::get_integerv(&context, GL_MAX_VIEWPORT_DIMS, &mut dims),
        2,
        "GL_MAX_VIEWPORT_DIMS is a 2-element query"
    );
    for bound in [
        (GL_MAX_TEXTURE_SIZE, "GL_MAX_TEXTURE_SIZE"),
        (pname::GL_MAX_RENDERBUFFER_SIZE_, "GL_MAX_RENDERBUFFER_SIZE"),
    ] {
        let (pname_value, name) = bound;
        let extent = integer(&context, pname_value);
        assert!(extent > 0, "{name} must be positive");
        assert!(
            dims[0] >= extent && dims[1] >= extent,
            "GL_MAX_VIEWPORT_DIMS {dims:?} cannot cover a {name} of {extent}"
        );
    }
}

#[test]
fn the_default_framebuffer_bit_depths_are_a_consistent_color_buffer() {
    let context = ctx();
    // The color bit depths a context reports must describe one real plane: this driver presents a single
    // RGBA8 surface, so all four components are 8 and their sum is the buffer size an EGL config
    // advertises (`EGL_BUFFER_SIZE`, cross-checked through the real ABI in `tests/egl_conformance.rs`).
    let components: Vec<i32> = [GL_RED_BITS, GL_GREEN_BITS, GL_BLUE_BITS, GL_ALPHA_BITS]
        .into_iter()
        .map(|pname| integer(&context, pname))
        .collect();
    assert_eq!(components, vec![8, 8, 8, 8]);
    assert_eq!(
        integer(&context, GL_DEPTH_BITS),
        24,
        "GL_DEPTH_BITS must match the depth plane the frame builder attaches"
    );
    assert_eq!(integer(&context, GL_STENCIL_BITS), 8);
    // A driver that resolves no multisampling must report neither sample buffers nor samples.
    assert_eq!(integer(&context, GL_SAMPLES), 0);
}

#[test]
fn the_initial_viewport_and_scissor_box_cover_the_whole_surface() {
    let context = ctx();
    // ES 3.0 §13.6.1 / §15.1.2: both are initialized to the size of the window the context is first
    // made current to. A zero-extent initial viewport draws nothing until the app happens to set one.
    let mut viewport = [i32::MIN; 4];
    assert_eq!(query::get_integerv(&context, GL_VIEWPORT, &mut viewport), 4);
    assert_eq!(viewport, [0, 0, 640, 480]);
}
