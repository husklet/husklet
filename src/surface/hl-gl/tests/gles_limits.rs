//! The capability-limit conformance battery: every advertised `glGetIntegerv` limit must be at or above
//! the GLES minimum for the profile the context reports, and the backing must really be there.
//!
//! The minima below are transcribed from the specification tables (ES 3.0 §6.2 tables 6.28–6.31, ES 3.1
//! §20), NOT from the driver. The second half proves the units are backed by DRIVING the driver (link a
//! 16-sampler stage, bind unit 31) instead of asserting on the constants. An ES2 context needs no table
//! of its own: every ES2 minimum is at or below the ES3.0 one for the limits advertised here.

use hl_gl::model::context::{ContextState, GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{query, record};

/// One advertised limit and its specification minimum per profile. `None` = the query does not exist in
/// that profile, so nothing is required of it.
struct SpecLimit {
    pname: u32,
    name: &'static str,
    es30: Option<i32>,
    es31: Option<i32>,
}

impl SpecLimit {
    /// The minimum required of this limit by the profile a context reporting `minor` advertises; `0`
    /// when the query does not exist in that profile (nothing is required of it).
    fn minimum(&self, minor: i32) -> i32 {
        let profile = if minor >= 1 { self.es31 } else { self.es30 };
        profile.unwrap_or(0)
    }
}

const fn both(pname: u32, name: &'static str, minimum: i32) -> SpecLimit {
    SpecLimit {
        pname,
        name,
        es30: Some(minimum),
        es31: Some(minimum),
    }
}

const fn es31_only(pname: u32, name: &'static str, minimum: i32) -> SpecLimit {
    SpecLimit {
        pname,
        name,
        es30: None,
        es31: Some(minimum),
    }
}

/// ES3.1-only pnames, repeated here so the table reads in isolation.
const GL_SUBPIXEL_BITS: u32 = 0x0D50;
const GL_MAX_COMPUTE_SHARED_MEMORY_SIZE: u32 = 0x8262;
const GL_MAX_COMPUTE_UNIFORM_COMPONENTS: u32 = 0x8263;
const GL_MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS: u32 = 0x8266;
const GL_MAX_UNIFORM_LOCATIONS: u32 = 0x826E;
const GL_MAX_VERTEX_ATTRIB_RELATIVE_OFFSET: u32 = 0x82D9;
const GL_MAX_VERTEX_ATTRIB_BINDINGS: u32 = 0x82DA;
const GL_MAX_VERTEX_ATTRIB_STRIDE: u32 = 0x82E5;
const GL_MAX_ELEMENT_INDEX: u32 = 0x8D6B;
const GL_MAX_SAMPLE_MASK_WORDS: u32 = 0x8E59;
const GL_MAX_IMAGE_UNITS: u32 = 0x8F38;
const GL_MAX_COMBINED_SHADER_OUTPUT_RESOURCES: u32 = 0x8F39;
const GL_MAX_COMBINED_IMAGE_UNIFORMS: u32 = 0x90CF;
const GL_MAX_VERTEX_SHADER_STORAGE_BLOCKS: u32 = 0x90D6;
const GL_MAX_FRAGMENT_SHADER_STORAGE_BLOCKS: u32 = 0x90DA;
const GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS: u32 = 0x90DB;
const GL_MAX_COMBINED_SHADER_STORAGE_BLOCKS: u32 = 0x90DC;
const GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS: u32 = 0x90DD;
const GL_MAX_SHADER_STORAGE_BLOCK_SIZE: u32 = 0x90DE;
const GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS: u32 = 0x90EB;
const GL_MAX_COLOR_TEXTURE_SAMPLES: u32 = 0x910E;
const GL_MAX_DEPTH_TEXTURE_SAMPLES: u32 = 0x910F;
const GL_MAX_INTEGER_SAMPLES: u32 = 0x9110;
const GL_MAX_COMPUTE_UNIFORM_BLOCKS: u32 = 0x91BB;
const GL_MAX_COMPUTE_TEXTURE_IMAGE_UNITS: u32 = 0x91BC;
const GL_MAX_COMPUTE_WORK_GROUP_COUNT: u32 = 0x91BE;
const GL_MAX_COMPUTE_WORK_GROUP_SIZE: u32 = 0x91BF;
const GL_MAX_ATOMIC_COUNTER_BUFFER_BINDINGS: u32 = 0x92DC;
const GL_MAX_FRAMEBUFFER_WIDTH: u32 = 0x9315;
const GL_MAX_FRAMEBUFFER_HEIGHT: u32 = 0x9316;
const GL_MAX_FRAMEBUFFER_SAMPLES: u32 = 0x9318;

/// The specification minimum for every capability limit this driver advertises.
const SPEC: &[SpecLimit] = &[
    // textures / renderbuffers
    both(GL_MAX_TEXTURE_SIZE, "MAX_TEXTURE_SIZE", 2048),
    both(
        GL_MAX_CUBE_MAP_TEXTURE_SIZE,
        "MAX_CUBE_MAP_TEXTURE_SIZE",
        2048,
    ),
    both(GL_MAX_RENDERBUFFER_SIZE, "MAX_RENDERBUFFER_SIZE", 2048),
    both(GL_MAX_3D_TEXTURE_SIZE, "MAX_3D_TEXTURE_SIZE", 256),
    both(GL_MAX_ARRAY_TEXTURE_LAYERS, "MAX_ARRAY_TEXTURE_LAYERS", 256),
    // texture image units
    both(GL_MAX_TEXTURE_IMAGE_UNITS, "MAX_TEXTURE_IMAGE_UNITS", 16),
    both(
        GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS,
        "MAX_VERTEX_TEXTURE_IMAGE_UNITS",
        16,
    ),
    both(
        GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS,
        "MAX_COMBINED_TEXTURE_IMAGE_UNITS",
        32,
    ),
    es31_only(
        GL_MAX_COMPUTE_TEXTURE_IMAGE_UNITS,
        "MAX_COMPUTE_TEXTURE_IMAGE_UNITS",
        16,
    ),
    // vertex input
    both(GL_MAX_VERTEX_ATTRIBS, "MAX_VERTEX_ATTRIBS", 16),
    es31_only(
        GL_MAX_VERTEX_ATTRIB_BINDINGS,
        "MAX_VERTEX_ATTRIB_BINDINGS",
        16,
    ),
    es31_only(
        GL_MAX_VERTEX_ATTRIB_STRIDE,
        "MAX_VERTEX_ATTRIB_STRIDE",
        2048,
    ),
    es31_only(
        GL_MAX_VERTEX_ATTRIB_RELATIVE_OFFSET,
        "MAX_VERTEX_ATTRIB_RELATIVE_OFFSET",
        2047,
    ),
    es31_only(GL_MAX_ELEMENT_INDEX, "MAX_ELEMENT_INDEX", (1 << 24) - 1),
    // program uniforms / varyings
    both(
        GL_MAX_VERTEX_UNIFORM_COMPONENTS,
        "MAX_VERTEX_UNIFORM_COMPONENTS",
        1024,
    ),
    both(
        GL_MAX_FRAGMENT_UNIFORM_COMPONENTS,
        "MAX_FRAGMENT_UNIFORM_COMPONENTS",
        896,
    ),
    both(
        GL_MAX_VERTEX_UNIFORM_VECTORS,
        "MAX_VERTEX_UNIFORM_VECTORS",
        256,
    ),
    both(
        GL_MAX_FRAGMENT_UNIFORM_VECTORS,
        "MAX_FRAGMENT_UNIFORM_VECTORS",
        224,
    ),
    both(GL_MAX_VARYING_VECTORS, "MAX_VARYING_VECTORS", 15),
    both(GL_MAX_VARYING_COMPONENTS, "MAX_VARYING_COMPONENTS", 60),
    both(
        GL_MAX_VERTEX_OUTPUT_COMPONENTS,
        "MAX_VERTEX_OUTPUT_COMPONENTS",
        64,
    ),
    both(
        GL_MAX_FRAGMENT_INPUT_COMPONENTS,
        "MAX_FRAGMENT_INPUT_COMPONENTS",
        60,
    ),
    es31_only(GL_MAX_UNIFORM_LOCATIONS, "MAX_UNIFORM_LOCATIONS", 1024),
    // uniform blocks
    both(
        GL_MAX_VERTEX_UNIFORM_BLOCKS,
        "MAX_VERTEX_UNIFORM_BLOCKS",
        12,
    ),
    both(
        GL_MAX_FRAGMENT_UNIFORM_BLOCKS,
        "MAX_FRAGMENT_UNIFORM_BLOCKS",
        12,
    ),
    es31_only(
        GL_MAX_COMPUTE_UNIFORM_BLOCKS,
        "MAX_COMPUTE_UNIFORM_BLOCKS",
        12,
    ),
    both(
        GL_MAX_COMBINED_UNIFORM_BLOCKS,
        "MAX_COMBINED_UNIFORM_BLOCKS",
        24,
    ),
    both(
        GL_MAX_UNIFORM_BUFFER_BINDINGS,
        "MAX_UNIFORM_BUFFER_BINDINGS",
        24,
    ),
    both(GL_MAX_UNIFORM_BLOCK_SIZE, "MAX_UNIFORM_BLOCK_SIZE", 16384),
    both(
        GL_MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS,
        "MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS",
        50176,
    ),
    both(
        GL_MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS,
        "MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS",
        50048,
    ),
    // compute (ES3.1)
    // DELIBERATE DEVIATION from the ES3.1 minimum of 1024, asserted as 0 rather than waived: the compute
    // path has NO default uniform block. `Program::link` returns before reflecting a uniform layout for a
    // compute source and `service::compute` binds only the `glBindBufferBase`d UBO/SSBO bindings, so a
    // `glUniform*` on a compute program would read zero forever. Advertising 1024 here would be exactly
    // the over-report this table exists to prevent; a compute shader that declares a default-block uniform
    // is refused at link (`UniformError::ComputeDefaultBlock`). This driver's ES3.1 profile is already
    // knowingly incomplete (no image units, no atomic counters) and is served only on explicit request.
    es31_only(
        GL_MAX_COMPUTE_UNIFORM_COMPONENTS,
        "MAX_COMPUTE_UNIFORM_COMPONENTS",
        0,
    ),
    es31_only(
        GL_MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS,
        "MAX_COMBINED_COMPUTE_UNIFORM_COMPONENTS",
        50176,
    ),
    es31_only(
        GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS,
        "MAX_COMPUTE_WORK_GROUP_INVOCATIONS",
        128,
    ),
    es31_only(
        GL_MAX_COMPUTE_SHARED_MEMORY_SIZE,
        "MAX_COMPUTE_SHARED_MEMORY_SIZE",
        16384,
    ),
    // shader storage (ES3.1)
    es31_only(
        GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS,
        "MAX_SHADER_STORAGE_BUFFER_BINDINGS",
        4,
    ),
    es31_only(
        GL_MAX_VERTEX_SHADER_STORAGE_BLOCKS,
        "MAX_VERTEX_SHADER_STORAGE_BLOCKS",
        0,
    ),
    es31_only(
        GL_MAX_FRAGMENT_SHADER_STORAGE_BLOCKS,
        "MAX_FRAGMENT_SHADER_STORAGE_BLOCKS",
        0,
    ),
    es31_only(
        GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS,
        "MAX_COMPUTE_SHADER_STORAGE_BLOCKS",
        4,
    ),
    es31_only(
        GL_MAX_COMBINED_SHADER_STORAGE_BLOCKS,
        "MAX_COMBINED_SHADER_STORAGE_BLOCKS",
        4,
    ),
    es31_only(
        GL_MAX_SHADER_STORAGE_BLOCK_SIZE,
        "MAX_SHADER_STORAGE_BLOCK_SIZE",
        1 << 27,
    ),
    es31_only(
        GL_MAX_COMBINED_SHADER_OUTPUT_RESOURCES,
        "MAX_COMBINED_SHADER_OUTPUT_RESOURCES",
        4,
    ),
    // framebuffers / multisample
    both(GL_MAX_COLOR_ATTACHMENTS, "MAX_COLOR_ATTACHMENTS", 4),
    both(GL_MAX_DRAW_BUFFERS, "MAX_DRAW_BUFFERS", 4),
    both(GL_MAX_SAMPLES, "MAX_SAMPLES", 4),
    es31_only(GL_MAX_FRAMEBUFFER_WIDTH, "MAX_FRAMEBUFFER_WIDTH", 2048),
    es31_only(GL_MAX_FRAMEBUFFER_HEIGHT, "MAX_FRAMEBUFFER_HEIGHT", 2048),
    es31_only(GL_MAX_FRAMEBUFFER_SAMPLES, "MAX_FRAMEBUFFER_SAMPLES", 4),
    es31_only(GL_MAX_SAMPLE_MASK_WORDS, "MAX_SAMPLE_MASK_WORDS", 1),
    es31_only(GL_MAX_COLOR_TEXTURE_SAMPLES, "MAX_COLOR_TEXTURE_SAMPLES", 1),
    es31_only(GL_MAX_DEPTH_TEXTURE_SAMPLES, "MAX_DEPTH_TEXTURE_SAMPLES", 1),
    es31_only(GL_MAX_INTEGER_SAMPLES, "MAX_INTEGER_SAMPLES", 1),
    both(GL_SUBPIXEL_BITS, "SUBPIXEL_BITS", 4),
    // transform feedback
    both(
        GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS,
        "MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS",
        4,
    ),
    both(
        GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS,
        "MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS",
        4,
    ),
    both(
        GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS,
        "MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS",
        64,
    ),
];

fn ctx(major: i32, minor: i32) -> GlContext {
    let mut c = GlContext::new();
    // The client version rides the EGL context's local state, exactly as `eglMakeCurrent` installs it.
    let mut requested = ContextState::with_version(major, minor, false);
    c.switch_state(&mut requested);
    c.set_surface(GlSurface {
        have: true,
        width: 800,
        height: 600,
    });
    c
}

fn integer(c: &GlContext, pname: u32) -> i32 {
    let mut out = [0i32; 4];
    assert!(query::get_integerv(c, pname, &mut out) >= 1);
    out[0]
}

/// The whole table, against the minimum for the profile the context actually reports. This is the guard:
/// an advertised limit can never again drop below the specification floor for the advertised version.
#[test]
fn every_advertised_limit_meets_the_spec_minimum_for_its_profile() {
    for (major, minor) in [(3, 0), (3, 1)] {
        let c = ctx(major, minor);
        assert_eq!(
            (integer(&c, GL_MAJOR_VERSION), integer(&c, GL_MINOR_VERSION)),
            (major, minor),
            "the context must report the profile it was created with"
        );
        for limit in SPEC {
            let minimum = limit.minimum(minor);
            let advertised = integer(&c, limit.pname);
            assert!(
                advertised >= minimum,
                "GL_{} ({:#x}) advertises {advertised}, below the GLES {major}.{minor} minimum {minimum}",
                limit.name,
                limit.pname
            );
        }
    }
}

/// `GL_MIN_PROGRAM_TEXEL_OFFSET` is a floor, not a ceiling: the spec requires it be at most -8.
#[test]
fn texel_offset_range_covers_the_spec_window() {
    let c = ctx(3, 1);
    assert!(integer(&c, GL_MIN_PROGRAM_TEXEL_OFFSET) <= -8);
    assert!(integer(&c, GL_MAX_PROGRAM_TEXEL_OFFSET) >= 7);
    // The uniform-buffer offset alignment is a MAXIMUM (256): a larger value would force an app into
    // padding the driver does not need.
    assert!(integer(&c, GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT) <= 256);
}

/// The per-dimension compute work-group limits are indexed queries; each dimension must clear its own
/// ES3.1 floor (65535 counts; 128/128/64 sizes).
#[test]
fn indexed_compute_work_group_limits_meet_the_spec_minimum() {
    let c = ctx(3, 1);
    for dimension in 0..3 {
        assert!(
            query::get_integer_indexed(&c, GL_MAX_COMPUTE_WORK_GROUP_COUNT, dimension) >= 65535,
            "work-group count dimension {dimension}"
        );
    }
    for (dimension, minimum) in [(0, 128), (1, 128), (2, 64)] {
        assert!(
            query::get_integer_indexed(&c, GL_MAX_COMPUTE_WORK_GROUP_SIZE, dimension) >= minimum,
            "work-group size dimension {dimension}"
        );
    }
}

/// The ES3.1 limits this driver deliberately reports as `0` because the FEATURE behind them is not
/// lowered at all (`glBindImageTexture` is a no-op; the translator rejects `atomic_uint`). Reporting 0 is
/// the honest answer; the residual gap is the ES3.1 profile claim itself, not these numbers.
#[test]
fn unbacked_es31_resource_limits_report_zero_rather_than_a_lie() {
    let c = ctx(3, 1);
    for (pname, name) in [
        (GL_MAX_IMAGE_UNITS, "MAX_IMAGE_UNITS"),
        (
            GL_MAX_COMBINED_IMAGE_UNIFORMS,
            "MAX_COMBINED_IMAGE_UNIFORMS",
        ),
        (
            GL_MAX_ATOMIC_COUNTER_BUFFER_BINDINGS,
            "MAX_ATOMIC_COUNTER_BUFFER_BINDINGS",
        ),
    ] {
        assert_eq!(
            integer(&c, pname),
            0,
            "GL_{name} must stay 0 while its feature is unlowered"
        );
    }
}

// ---- the backing, driven rather than asserted -----------------------------------------------------

fn sampler_stage(count: usize, fragment: bool) -> String {
    let mut source = String::new();
    if fragment {
        source.push_str("precision mediump float;\n");
    }
    for index in 0..count {
        source.push_str(&format!("uniform sampler2D uTex{index};\n"));
    }
    source.push_str("void main(){\n  vec4 acc = vec4(0.0);\n");
    for index in 0..count {
        source.push_str(&format!(
            "  acc += texture2D(uTex{index}, vec2(0.5, 0.5));\n"
        ));
    }
    source.push_str(if fragment {
        "  gl_FragColor = acc;\n}\n"
    } else {
        "  gl_Position = acc;\n}\n"
    });
    source
}

fn link(c: &mut GlContext, vs_src: &str, fs_src: &str) -> bool {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, vs_src);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, fs_src);
    record::compile_shader(c, fs);
    let program = record::create_program(c);
    record::attach_shader(c, program, vs);
    record::attach_shader(c, program, fs);
    record::link_program(c, program)
}

/// A program that uses the advertised per-stage sampler count in BOTH stages must link — the advertised
/// `GL_MAX_*_TEXTURE_IMAGE_UNITS` is exactly the link-time ceiling.
#[test]
fn a_program_using_the_advertised_sampler_counts_links() {
    let mut c = ctx(3, 1);
    let vertex_units = integer(&c, GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS) as usize;
    let fragment_units = integer(&c, GL_MAX_TEXTURE_IMAGE_UNITS) as usize;
    assert!(
        link(
            &mut c,
            &sampler_stage(vertex_units, false),
            &sampler_stage(fragment_units, true)
        ),
        "a program with {vertex_units} vertex + {fragment_units} fragment samplers must link"
    );
}

/// Every unit up to the advertised combined count must be selectable by `glActiveTexture` and hold a
/// binding — the bank really is that wide, so a sampler pointed at the last unit resolves.
#[test]
fn every_advertised_combined_texture_unit_binds() {
    let mut c = ctx(3, 1);
    let units = integer(&c, GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS) as u32;
    for unit in 0..units {
        let texture = c.textures.gen();
        c.active_texture(GL_TEXTURE0 + unit);
        record::bind_texture(&mut c, GL_TEXTURE_2D, texture);
        assert_eq!(
            integer(&c, GL_ACTIVE_TEXTURE) as u32,
            GL_TEXTURE0 + unit,
            "unit {unit} must be selectable"
        );
        assert_eq!(
            integer(&c, GL_TEXTURE_BINDING_2D) as u32,
            texture,
            "unit {unit} must hold its binding"
        );
        assert_eq!(c.take_gl_error(), GL_NO_ERROR, "unit {unit}");
    }
}

#[test]
fn texture_units_keep_independent_2d_and_cube_bindings() {
    let mut c = ctx(2, 0);
    let texture_2d = c.textures.gen();
    let texture_cube = c.textures.gen();

    record::bind_texture(&mut c, GL_TEXTURE_2D, texture_2d);
    record::bind_texture(&mut c, GL_TEXTURE_CUBE_MAP, texture_cube);
    record::tex_parameter_target(&mut c, GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    record::tex_parameter_target(
        &mut c,
        GL_TEXTURE_CUBE_MAP,
        GL_TEXTURE_MIN_FILTER,
        GL_LINEAR,
    );

    assert_eq!(integer(&c, GL_TEXTURE_BINDING_2D) as u32, texture_2d);
    assert_eq!(
        integer(&c, GL_TEXTURE_BINDING_CUBE_MAP) as u32,
        texture_cube
    );
    assert_eq!(
        query::get_tex_parameteriv(&c, GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER),
        GL_NEAREST as i32
    );
    assert_eq!(
        query::get_tex_parameteriv(&c, GL_TEXTURE_CUBE_MAP, GL_TEXTURE_MIN_FILTER),
        GL_LINEAR as i32
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// A texture at the advertised `GL_MAX_TEXTURE_SIZE` must be accepted, and one edge past it rejected:
/// the advertised ceiling is the same one the record path validates against.
#[test]
fn the_advertised_texture_ceiling_is_the_enforced_one() {
    let mut c = ctx(3, 1);
    let ceiling = integer(&c, GL_MAX_TEXTURE_SIZE);
    let texture = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, texture);
    record::tex_image_2d(&mut c, ceiling, 1, &[]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR, "the ceiling itself");
    record::tex_image_2d(&mut c, ceiling + 1, 1, &[]);
    assert_eq!(
        c.take_gl_error(),
        GL_INVALID_VALUE,
        "one edge past the advertised ceiling"
    );
}

/// The advertised texture edge must not exceed what the HOST will actually create: the executor's
/// `Capabilities::max_texture_2d` is enforced by `hl_gpu`'s runtime validation, so a larger advertised
/// limit is a promise the backend refuses at submit time.
#[test]
fn the_advertised_texture_ceiling_fits_the_executor_capability() {
    use hl_gpu::GpuExecutor;
    let host = hl_gpu::CpuExecutor::new().capabilities().max_texture_2d as i32;
    let c = ctx(3, 1);
    for pname in [
        GL_MAX_TEXTURE_SIZE,
        GL_MAX_CUBE_MAP_TEXTURE_SIZE,
        GL_MAX_RENDERBUFFER_SIZE,
    ] {
        let advertised = integer(&c, pname);
        assert!(
            advertised <= host,
            "limit {pname:#x} advertises {advertised}, above the executor's {host}"
        );
    }
}

/// The advertised uniform ceiling versus what the LINKER accepts — both directions. An advertised limit
/// the linker refuses is an over-report; a linker that accepts more than was advertised is a silently
/// unadvertised capability. `glLinkProgram` is the only place a uniform budget is enforced, so this is
/// the executor-facing edge of `GL_MAX_{VERTEX,FRAGMENT}_UNIFORM_VECTORS`.
#[test]
fn the_advertised_uniform_ceiling_is_exactly_what_the_linker_accepts() {
    use hl_gl::adapter::glsl;

    let c = ctx(3, 1);
    for (vectors, components) in [
        (GL_MAX_VERTEX_UNIFORM_VECTORS, GL_MAX_VERTEX_UNIFORM_COMPONENTS),
        (
            GL_MAX_FRAGMENT_UNIFORM_VECTORS,
            GL_MAX_FRAGMENT_UNIFORM_COMPONENTS,
        ),
    ] {
        assert_eq!(integer(&c, components), integer(&c, vectors) * 4);
    }
    let advertised = integer(&c, GL_MAX_VERTEX_UNIFORM_VECTORS) as usize;

    // Exactly the advertised count links, in EITHER stage and in BOTH at once (the two are flattened into
    // one std140 block, so "both at once" is the case a per-stage-only budget would miss).
    let at_limit = format!("uniform vec4 v[{advertised}];\nvoid main(){{ gl_Position = v[0]; }}\n");
    let fragment = format!("uniform vec4 f[{advertised}];\nvoid main(){{ gl_FragColor = f[0]; }}\n");
    assert!(
        glsl::StageSources::new(&at_limit, &fragment)
            .uniform_layout()
            .is_ok(),
        "both stages at the advertised ceiling must link"
    );

    // One vector past it is refused, loudly and by name.
    let over = format!(
        "uniform vec4 v[{}];\nvoid main(){{ gl_Position = v[0]; }}\n",
        advertised + 1
    );
    let error = glsl::StageSources::new(&over, "void main(){}")
        .uniform_layout()
        .expect_err("one vector past the advertised ceiling must be refused");
    assert!(
        matches!(error, glsl::UniformError::StageComponents { stage: "vertex", .. }),
        "expected a component-count diagnostic, got {error}"
    );

    // WORST-CASE std140 expansion: a scalar array costs a full 16-byte stride per component, so a program
    // at the advertised component count in both stages is the largest block a link can ever produce. It
    // must still fit the enforced combined budget — otherwise the advertised limit is reachable only for
    // shaders that happen to pack well, which is not a limit at all.
    let components = advertised * 4;
    assert_eq!(2 * components * 16, glsl::MAX_COMBINED_UNIFORM_BYTES);
    let scalars = |name: &str, body: &str| {
        format!("uniform float {name}[{components}];\nvoid main(){{ {body} }}\n")
    };
    assert!(
        glsl::StageSources::new(
            &scalars("v", "gl_Position = vec4(v[0]);"),
            &scalars("f", "gl_FragColor = vec4(f[0]);"),
        )
        .uniform_layout()
        .is_ok(),
        "the advertised ceiling must hold even for the worst-case std140 layout"
    );
}

/// COMPUTE advertises ZERO default-block uniform components, and that zero is enforced: the compute path
/// reflects no uniform layout and binds only `glBindBufferBase`d buffers, so a `glUniform*` there would
/// read zero forever. The link must fail instead — a silent zero is the defect this pins.
#[test]
fn a_compute_default_block_uniform_is_refused_because_none_is_carried() {
    use hl_gl::adapter::glsl;

    let c = ctx(3, 1);
    assert_eq!(integer(&c, GL_MAX_COMPUTE_UNIFORM_COMPONENTS), 0);

    let rejected = glsl::compute_default_block_uniform(
        "#version 310 es\nuniform float scale;\nvoid main(){}\n",
    );
    assert_eq!(rejected, Ok(Some("scale".into())));

    // A `glBindBufferBase`d BLOCK is a different, working path and must keep linking.
    let accepted = glsl::compute_default_block_uniform(
        "#version 310 es\nlayout(std140, binding = 0) uniform Params { float scale; };\nvoid main(){}\n",
    );
    assert_eq!(accepted, Ok(None));
}
