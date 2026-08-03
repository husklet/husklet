use hl_gl::model::context::GlContext;
use hl_gl::model::glconst::*;
use hl_gl::service::{intro, query, record};

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
fn sampler_vector_writes_stop_at_the_selected_uniform_array() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "void main(){gl_Position=vec4(0.0);}",
        "uniform sampler2D single; uniform sampler2D pair[2]; uniform sampler2D tail;\n\
         void main(){gl_FragColor=texture2D(single,vec2(0.0))+texture2D(pair[1],vec2(0.0))+texture2D(tail,vec2(0.0));}",
    );
    record::use_program(&mut context, program);
    let single = query::uniform_location(&context, program, "single");
    let pair = query::uniform_location(&context, program, "pair");
    let pair_one = query::uniform_location(&context, program, "pair[1]");

    record::set_uniform(
        &mut context,
        single,
        record::UniformSetter::Int(1),
        2,
        &[7_i32.to_le_bytes(), 8_i32.to_le_bytes()].concat(),
    );
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 0, 0, 0]);

    record::set_uniform(
        &mut context,
        pair,
        record::UniformSetter::Int(1),
        3,
        &[1_i32.to_le_bytes(), 2_i32.to_le_bytes(), 9_i32.to_le_bytes()].concat(),
    );
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 1, 2, 0]);

    record::set_uniform(
        &mut context,
        pair_one,
        record::UniformSetter::Int(1),
        2,
        &[5_i32.to_le_bytes(), 6_i32.to_le_bytes()].concat(),
    );
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 1, 5, 0]);
}

#[test]
fn program_sampler_vector_writes_stop_at_the_selected_uniform_array() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "void main(){gl_Position=vec4(0.0);}",
        "uniform sampler2D single; uniform sampler2D pair[2]; uniform sampler2D tail;\n\
         void main(){gl_FragColor=texture2D(single,vec2(0.0))+texture2D(pair[1],vec2(0.0))+texture2D(tail,vec2(0.0));}",
    );
    let single = query::uniform_location(&context, program, "single");
    let pair = query::uniform_location(&context, program, "pair");
    let pair_one = query::uniform_location(&context, program, "pair[1]");

    record::program_uniform_i32_at(&mut context, program, single, 2, &[7, 8]);
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 0, 0, 0]);

    record::program_uniform_i32_at(&mut context, program, pair, 3, &[1, 2, 9]);
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 1, 2, 0]);

    record::program_uniform_i32_at(&mut context, program, pair_one, 2, &[5, 6]);
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 1, 5, 0]);

    record::program_uniform_i32_at(&mut context, program, pair, -1, &[]);
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(context.programs.program(program).unwrap().samp_units, [0, 1, 5, 0]);
}

#[test]
fn uniform_setters_validate_program_location_type_width_and_count() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "uniform vec4 vector; uniform ivec4 integer; uniform mat4 matrix; uniform float values[2];\nvoid main(){gl_Position=vector+vec4(integer)+matrix*vec4(values[1]);}",
        "uniform sampler2D image;\nvoid main(){gl_FragColor=texture2D(image,vec2(0));}",
    );
    let vector = query::uniform_location(&context, program, "vector");
    let integer = query::uniform_location(&context, program, "integer");
    let matrix = query::uniform_location(&context, program, "matrix");
    let values = query::uniform_location(&context, program, "values");
    let image = query::uniform_location(&context, program, "image");
    let zeros = [0_u8; 64];

    record::set_uniform(
        &mut context,
        -1,
        record::UniformSetter::Float(1),
        1,
        &zeros[..4],
    );
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::set_uniform(
        &mut context,
        -2,
        record::UniformSetter::Float(1),
        1,
        &zeros[..4],
    );
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::set_uniform(
        &mut context,
        vector,
        record::UniformSetter::Float(4),
        1,
        &zeros[..16],
    );
    assert_eq!(
        context.take_gl_error(),
        GL_INVALID_OPERATION,
        "no current program"
    );

    record::use_program(&mut context, program);
    record::set_uniform(
        &mut context,
        -1,
        record::UniformSetter::Float(1),
        1,
        &zeros[..4],
    );
    assert_eq!(context.take_gl_error(), GL_NO_ERROR);
    for (location, setter) in [
        (vector, record::UniformSetter::Float(3)),
        (vector, record::UniformSetter::Int(4)),
        (integer, record::UniformSetter::Int(3)),
        (matrix, record::UniformSetter::Matrix(3)),
        (image, record::UniformSetter::Float(1)),
    ] {
        record::set_uniform(&mut context, location, setter, 1, &zeros);
        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION, "{setter:?}");
    }
    record::set_uniform(
        &mut context,
        vector,
        record::UniformSetter::Float(4),
        2,
        &zeros,
    );
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::set_uniform(
        &mut context,
        values,
        record::UniformSetter::Float(1),
        3,
        &zeros[..12],
    );
    assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
    record::set_uniform(
        &mut context,
        values,
        record::UniformSetter::Float(1),
        -1,
        &[],
    );
    assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);

    for (location, setter, count, bytes) in [
        (vector, record::UniformSetter::Float(4), 1, &zeros[..16]),
        (integer, record::UniformSetter::Int(4), 1, &zeros[..16]),
        (matrix, record::UniformSetter::Matrix(4), 1, &zeros[..64]),
        (values, record::UniformSetter::Float(1), 2, &zeros[..8]),
        (image, record::UniformSetter::Int(1), 1, &zeros[..4]),
    ] {
        record::set_uniform(&mut context, location, setter, count, bytes);
        assert_eq!(context.take_gl_error(), GL_NO_ERROR, "{setter:?}");
    }
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

/// A matrix uniform reports the column stride an application must step by, not zero.
///
/// `GL_UNIFORM_MATRIX_STRIDE` was hardcoded to zero for every type. This is not a description an
/// application can ignore: laying out a std140 block means writing column `n` at
/// `offset + n * matrix_stride`, so a zero stacks all of them at the offset and the program overwrites
/// its own uniform with the last column. The block genuinely is std140 here — every column is padded to
/// sixteen bytes, which is where the type sizes come from — so the honest answer is sixteen.
#[test]
fn a_matrix_uniform_reports_its_std140_column_stride() {
    for declared in [
        "mat2", "mat3", "mat4", "mat2x3", "mat2x4", "mat3x2", "mat3x4", "mat4x2", "mat4x3",
    ] {
        let mut context = GlContext::new();
        let program = program(
            &mut context,
            &format!("uniform {declared} m;\nvoid main(){{gl_Position=vec4(m[0][0]);}}"),
            "void main(){gl_FragColor=vec4(1.0);}",
        );
        assert_eq!(
            intro::active_uniformsiv(&context, program, 0, GL_UNIFORM_MATRIX_STRIDE),
            Some(16),
            "{declared} must report the stride between its columns"
        );
    }

    // A non-matrix uniform has no columns, and GL reports zero for it. Asserted so the fix cannot be a
    // constant sixteen applied to everything.
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "uniform vec4 v;\nvoid main(){gl_Position=v;}",
        "void main(){gl_FragColor=vec4(1.0);}",
    );
    assert_eq!(
        intro::active_uniformsiv(&context, program, 0, GL_UNIFORM_MATRIX_STRIDE),
        Some(0),
        "a vector has no column stride"
    );
}

#[test]
fn aggregate_uniforms_reflect_real_leaf_names_indices_and_layout() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "struct Inner { int scalar; };\n\
         struct Outer { int before; Inner inner; int values[2]; };\n\
         uniform Outer u_var[2];\n\
         void main(){gl_Position=vec4(float(u_var[1].inner.scalar+u_var[0].values[1]));}",
        "void main(){gl_FragColor=vec4(1.0);}",
    );

    let expected = [
        ("u_var[0].before", 0, 0, 0),
        ("u_var[0].inner.scalar", 1, 16, 0),
        ("u_var[0].values[0]", 2, 32, 16),
        ("u_var[1].before", 3, 64, 0),
        ("u_var[1].inner.scalar", 4, 80, 0),
        ("u_var[1].values[0]", 5, 96, 16),
    ];
    for (name, index, offset, stride) in expected {
        assert_eq!(intro::uniform_index(&context, program, name), index);
        assert_eq!(
            intro::active_uniformsiv(&context, program, index, GL_UNIFORM_OFFSET),
            Some(offset)
        );
        assert_eq!(
            intro::active_uniformsiv(&context, program, index, GL_UNIFORM_ARRAY_STRIDE),
            Some(stride)
        );
    }

    for index in [2, 5] {
        let active = query::active_uniform(&context, program, index).expect("array leaf");
        assert_eq!(active.size, 2);
        assert_eq!(
            intro::active_uniformsiv(&context, program, index, GL_UNIFORM_SIZE),
            Some(2)
        );
    }
}

#[test]
fn sampler_struct_leaves_have_real_locations_and_units() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "void main(){gl_Position=vec4(0.0);}",
        "struct Images { sampler2D image; samplerCube cube; };\n\
         uniform Images u_var;\n\
         void main(){gl_FragColor=texture2D(u_var.image,vec2(0.5))+textureCube(u_var.cube,vec3(1.0));}",
    );

    for (name, index, gl_type) in [
        ("u_var.image", 0, GL_SAMPLER_2D),
        ("u_var.cube", 1, GL_SAMPLER_CUBE),
    ] {
        assert_eq!(intro::uniform_index(&context, program, name), index);
        assert_eq!(
            query::uniform_location(&context, program, name),
            index as i32
        );
        let active = query::active_uniform(&context, program, index).expect("sampler leaf");
        assert_eq!((active.name.as_str(), active.gl_type), (name, gl_type));
    }

    record::use_program(&mut context, program);
    record::uniform_i32_at(&mut context, 0, &[4]);
    record::uniform_i32_at(&mut context, 1, &[7]);
    assert_eq!(intro::get_sampler_unit(&context, program, 0), Some(4));
    assert_eq!(intro::get_sampler_unit(&context, program, 1), Some(7));
}

#[test]
fn mixed_struct_data_and_sampler_leaves_share_the_public_location_space() {
    let mut context = GlContext::new();
    let program = program(
        &mut context,
        "void main(){gl_Position=vec4(0.0);}",
        "struct Material { vec4 tint; sampler2D image; }; uniform Material material; \
         void main(){gl_FragColor=material.tint+texture2D(material.image,vec2(0.5));}",
    );
    assert_eq!(
        query::uniform_location(&context, program, "material.tint"),
        0
    );
    assert_eq!(
        query::uniform_location(&context, program, "material.image"),
        1
    );
    record::use_program(&mut context, program);
    record::uniform_i32_at(&mut context, 1, &[6]);
    assert_eq!(intro::get_sampler_unit(&context, program, 1), Some(6));
}
