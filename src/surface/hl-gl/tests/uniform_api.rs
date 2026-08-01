use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::*;
use hl_gl::service::{query, record};

fn program(context: &mut GlContext, vertex: &str, fragment: &str) -> u32 {
    let vs = record::create_shader(context, GL_VERTEX_SHADER);
    record::shader_source(context, vs, vertex);
    record::compile_shader(context, vs);
    let fs = record::create_shader(context, GL_FRAGMENT_SHADER);
    record::shader_source(context, fs, fragment);
    record::compile_shader(context, fs);
    let program = record::create_program(context);
    record::attach_shader(context, program, vs);
    record::attach_shader(context, program, fs);
    assert!(record::link_program(context, program));
    program
}

#[test]
fn locations_are_unique_and_array_elements_are_addressable() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "uniform int mode;\nuniform float weights[3];\nvoid main(){gl_Position=vec4(weights[1]);}",
        "uniform sampler2D images[2];\nvoid main(){gl_FragColor=texture2D(images[1],vec2(0));}",
    );

    for (name, location) in [
        ("mode", 0),
        ("weights", 1),
        ("weights[0]", 1),
        ("weights[1]", 2),
        ("weights[2]", 3),
        ("images", 4),
        ("images[0]", 4),
        ("images[1]", 5),
    ] {
        assert_eq!(query::uniform_location(&context, program, name), location);
    }
    assert_eq!(query::uniform_location(&context, program, "weights[3]"), -1);
    assert_eq!(query::uniform_location(&context, program, "images[2]"), -1);
}

#[test]
fn active_arrays_report_base_name_and_element_count() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "uniform float weights[3];\nvoid main(){gl_Position=vec4(weights[1]);}",
        "uniform sampler2D images[2];\nvoid main(){gl_FragColor=texture2D(images[1],vec2(0));}",
    );

    let weights = query::active_uniform(&context, program, 0).expect("weights");
    assert_eq!((weights.name.as_str(), weights.size), ("weights[0]", 3));
    let images = query::active_uniform(&context, program, 1).expect("images");
    assert_eq!((images.name.as_str(), images.size), ("images[0]", 2));
}

#[test]
fn integer_and_sampler_setters_dispatch_by_location() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "uniform int mode;\nuniform float weights[2];\nvoid main(){gl_Position=vec4(weights[1]+float(mode));}",
        "uniform sampler2D images[2];\nvoid main(){gl_FragColor=texture2D(images[1],vec2(0));}",
    );
    record::use_program(&mut context, program);

    record::uniform_i32_at(&mut context, 0, &[7]);
    record::uniform_at(&mut context, 2, &9.5f32.to_le_bytes());
    record::uniform_i32_at(&mut context, 3, &[4, 6]);

    let program = context.programs.program(program).expect("program");
    assert_eq!(
        i32::from_le_bytes(program.ubuf[0..4].try_into().unwrap()),
        7
    );
    assert_eq!(
        f32::from_le_bytes(program.ubuf[32..36].try_into().unwrap()),
        9.5
    );
    assert_eq!(program.samp_units, vec![4, 6]);
}

#[test]
fn mandatory_uniform_limits_are_nonzero_and_coherent() {
    let context = GlContext::new();
    let mut values = [0; 4];
    for name in [
        GL_MAX_UNIFORM_BLOCK_SIZE,
        GL_MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS,
        GL_MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS,
    ] {
        assert_eq!(query::get_integerv(&context, name, &mut values), 1);
        assert!(values[0] > 0, "{name:#x} was not advertised");
    }
    query::get_integerv(&context, GL_MAX_UNIFORM_BLOCK_SIZE, &mut values);
    assert!(values[0] as usize <= hl_gl::adapter::glsl::MAX_COMBINED_UNIFORM_BYTES);
}

/// Every matrix shape reports its own type, not the scalar the fallback hands out.
///
/// `GlType` mapped only `mat2`, `mat3` and `mat4`, so the six non-square shapes fell through to
/// `_ => GL_FLOAT` — the same silent default in a different costume. `glGetActiveUniform` and
/// `glGetActiveAttrib` described a `mat3x2` as a `float`, while the value itself was delivered correctly
/// column for column. That partition is what makes it diagnosable at a glance: every square shape passed
/// and every non-square one failed, and the failures were reflection tests rather than rendering ones.
///
/// `matCxR` is C columns of R rows and the enum spells the same order, so a transposed mapping would show
/// up here as `mat2x3` reporting `GL_FLOAT_MAT3x2`.
#[test]
fn every_matrix_shape_reports_its_own_type() {
    for (declared, expected) in [
        ("mat2", GL_FLOAT_MAT2),
        ("mat3", GL_FLOAT_MAT3),
        ("mat4", GL_FLOAT_MAT4),
        ("mat2x3", GL_FLOAT_MAT2x3),
        ("mat2x4", GL_FLOAT_MAT2x4),
        ("mat3x2", GL_FLOAT_MAT3x2),
        ("mat3x4", GL_FLOAT_MAT3x4),
        ("mat4x2", GL_FLOAT_MAT4x2),
        ("mat4x3", GL_FLOAT_MAT4x3),
    ] {
        let mut context = GlContext::new();
        let program = program(
            &mut context,
            &format!("uniform {declared} m;\nvoid main(){{gl_Position=vec4(m[0][0]);}}"),
            "void main(){gl_FragColor=vec4(1.0);}",
        );
        let reflected = query::active_uniform(&context, program, 0)
            .unwrap_or_else(|| panic!("{declared} is an active uniform"));
        assert_eq!(
            reflected.gl_type, expected,
            "{declared} must report its own type, not 0x{:04X}",
            reflected.gl_type
        );
    }
}
