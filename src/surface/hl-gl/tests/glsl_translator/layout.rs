use super::*;

#[test]
fn int_and_matrix_uniform_types_layout_without_panicking() {
    // Exercise the full type table in uni_layout (ivec/uvec/matNxM) — must produce monotonic offsets.
    let fs = "uniform int uA;\nuniform ivec2 uB;\nuniform uvec3 uC;\nuniform mat3 uD;\nuniform mat2 uE;\n\
              void main(){ gl_FragColor = vec4(float(uA)); }\n";
    let (unis, total) = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect("supported uniform layout");
    assert_eq!(unis.len(), 5);
    // Offsets are non-decreasing and each member fits before the next.
    for w in unis.windows(2) {
        assert!(
            w[1].off >= w[0].off + w[0].sz,
            "overlapping members: {:?}",
            unis
        );
    }
    assert!(total >= unis.last().unwrap().off + unis.last().unwrap().sz);
    assert_eq!(total % 16, 0, "block total is 16-byte aligned");
}

#[test]
fn std140_two_row_matrices_use_a_sixteen_byte_column_stride() {
    let fs = "uniform mat2 a;\nuniform mat3x2 b;\nuniform mat4x2 c;\nvoid main(){}\n";
    let (uniforms, total) = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect("matrix types are supported");

    assert_eq!(
        uniforms
            .iter()
            .map(|uniform| (uniform.name.as_str(), uniform.off, uniform.sz))
            .collect::<Vec<_>>(),
        vec![("a", 0, 32), ("b", 32, 48), ("c", 80, 64)]
    );
    assert_eq!(total, 144);
}

#[test]
fn struct_uniform_reflects_leaf_names_and_std140_offsets() {
    // Reduced from
    // dEQP-GLES3.functional.uniform_api.info_query.indices_active_uniformsiv
    //     .array_in_struct.int_ivec4_both.
    // GLES reflects the basic leaves of a struct uniform, not one opaque `structType` entry. Arrays retain
    // one active-uniform declaration (`[0]` is added by the query boundary) and a 16-byte std140 stride.
    let fs = "#version 300 es\nprecision highp float;\n\
              struct structType { int m0; ivec4 m1; int m2[3]; ivec4 m3[3]; };\n\
              uniform structType u_var;\nout vec4 color;\n\
              void main(){ color = vec4(float(u_var.m0 + u_var.m2[2])) + vec4(u_var.m1 + u_var.m3[1]); }\n";
    let (uniforms, total) = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect("a legal default-block struct uniform must link");

    assert_eq!(
        uniforms
            .iter()
            .map(|uniform| {
                (
                    uniform.name.as_str(),
                    uniform.ty.as_str(),
                    uniform.arr,
                    uniform.off,
                    uniform.sz,
                )
            })
            .collect::<Vec<_>>(),
        [
            ("u_var.m0", "int", 0, 0, 4),
            ("u_var.m1", "ivec4", 0, 16, 16),
            ("u_var.m2", "int", 3, 32, 48),
            ("u_var.m3", "ivec4", 3, 80, 48),
        ]
    );
    assert_eq!(total, 128);
    assert_eq!(
        glsl::StageSources::new("void main(){}", fs)
            .uniform_decls()
            .iter()
            .map(|uniform| uniform.name.as_str())
            .collect::<Vec<_>>(),
        ["u_var.m0", "u_var.m1", "u_var.m2", "u_var.m3"]
    );

    let (_, translated) = glsl::StageSources::new("void main(){}", fs).translate_render();
    assert!(!translated.contains("structType u_var"), "{translated}");
    assert!(translated.contains("int u_var_m2[3]"), "{translated}");
    assert!(translated.contains("u_var_m3[1]"), "{translated}");
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn array_of_struct_reflects_each_element_member_and_stride() {
    let fs = "#version 300 es\nstruct structType { int m0; ivec4 m1; };\n\
              uniform structType u_var[3];\nout vec4 color;\n\
              void main(){ color = vec4(float(u_var[1].m0)) + vec4(u_var[2].m1); }\n";
    let (uniforms, total) = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect("an array of legal data structures must link");

    assert_eq!(
        uniforms
            .iter()
            .map(|uniform| (uniform.name.as_str(), uniform.arr, uniform.off, uniform.sz))
            .collect::<Vec<_>>(),
        [
            ("u_var[0].m0", 0, 0, 4),
            ("u_var[0].m1", 0, 16, 16),
            ("u_var[1].m0", 0, 32, 4),
            ("u_var[1].m1", 0, 48, 16),
            ("u_var[2].m0", 0, 64, 4),
            ("u_var[2].m1", 0, 80, 16),
        ]
    );
    assert_eq!(total, 96);
}

#[test]
fn nested_structures_align_each_aggregate_and_preserve_leaf_arrays() {
    let fs = "#version 300 es\nstruct Inner { int scalar; };\n\
              struct Outer { int before; Inner inner; int values[2]; };\n\
              uniform Outer u_var;\nout vec4 color;\n\
              void main(){ color = vec4(float(u_var.before + u_var.inner.scalar + u_var.values[1])); }\n";
    let (uniforms, total) = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect("nested legal data structures must link");

    assert_eq!(
        uniforms
            .iter()
            .map(|uniform| (uniform.name.as_str(), uniform.arr, uniform.off, uniform.sz))
            .collect::<Vec<_>>(),
        [
            ("u_var.before", 0, 0, 4),
            ("u_var.inner.scalar", 0, 16, 4),
            ("u_var.values", 2, 32, 32),
        ]
    );
    assert_eq!(total, 64);
}

#[test]
fn sampler_only_struct_lowers_leaves_to_standalone_bindings() {
    let fs = "#version 300 es\nprecision highp float;\n\
              struct Images { sampler2D image; samplerCube cube; };\n\
              uniform Images u_var;\nout vec4 color;\n\
              void main(){ color = texture(u_var.image, vec2(0.5)) + texture(u_var.cube, vec3(0.0, 0.0, 1.0)); }\n";
    let (uniforms, total) = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect("opaque aggregate leaves are standalone uniforms");
    assert!(uniforms.is_empty());
    assert_eq!(total, 0);
    assert_eq!(
        glsl::StageSources::new("void main(){}", fs)
            .sampler_decls()
            .iter()
            .map(|sampler| (sampler.name.as_str(), sampler.ty.as_str()))
            .collect::<Vec<_>>(),
        [("u_var.image", "sampler2D"), ("u_var.cube", "samplerCube")]
    );

    let (_, translated) = glsl::StageSources::new("void main(){}", fs).translate_render();
    assert!(!translated.contains("Images u_var"));
    assert!(translated.contains("u_var_image_hltex"), "{translated}");
    assert!(
        translated.contains("sampler2D(u_var_image_hltex, u_var_image_hlsmp)"),
        "{translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn aggregate_non_square_matrices_flatten_before_host_validation() {
    let fs = "#version 300 es\nstruct Matrices { mat3x2 wide; mat2 square[2]; };\n\
              uniform Matrices u_var;\nout vec4 color;\n\
              void main(){ color=vec4(u_var.wide[0][0]+u_var.square[1][0][0]); }\n";
    let (_, translated) = glsl::StageSources::new("void main(){}", fs).translate_render();
    assert!(!translated.contains("Matrices u_var"), "{translated}");
    assert!(
        translated.contains("vec4 u_var_wide_hle0_hlc2"),
        "{translated}"
    );
    assert!(
        translated.contains("mat3x2(u_var_wide_hle0_hlc0.xy"),
        "{translated}"
    );
    assert!(
        translated.contains("u_var_square_hle1_hlc1"),
        "{translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn aggregate_booleans_use_host_shareable_integer_storage() {
    let fs = "#version 300 es\nstruct Flags { bool enabled; bvec4 mask[2]; };\n\
              uniform Flags u_var;\nout vec4 color;\n\
              void main(){ color=(u_var.enabled && all(u_var.mask[1])) ? vec4(1.0) : vec4(0.0); }\n";
    let (_, translated) = glsl::StageSources::new("void main(){}", fs).translate_render();
    assert!(translated.contains("uint u_var_enabled"), "{translated}");
    assert!(translated.contains("uvec4 u_var_mask[2]"), "{translated}");
    assert!(translated.contains("u_var_enabled != 0u"), "{translated}");
    assert!(
        translated.contains("notEqual(u_var_mask[1]"),
        "{translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn conflicting_cross_stage_uniform_declarations_are_rejected() {
    let error = glsl::StageSources::new(
        "uniform float value;\nvoid main(){ gl_Position = vec4(value); }\n",
        "uniform vec4 value;\nvoid main(){ gl_FragColor = value; }\n",
    )
    .uniform_layout()
    .expect_err("one uniform name cannot have two layouts");
    assert_eq!(
        error,
        glsl::UniformError::ConflictingDeclaration("value".into())
    );
}

#[test]
fn conflicting_uniform_declarations_are_rejected() {
    let vertex = "uniform vec4 value;\n\
                  uniform float value;\n\
                  void main(){ gl_Position = vec4(value); }\n";
    assert_eq!(
        glsl::StageSources::new(vertex, "void main(){}").uniform_layout(),
        Err(glsl::UniformError::ConflictingDeclaration("value".into()))
    );
}

/// Opposite `#if`/`#else` arms are NOT a conflict: preprocessing selects one arm, so only the live
/// declaration is reflected (GLSL ES 1.00 §3.4).
#[test]
fn only_the_live_conditional_uniform_arm_is_reflected() {
    let vertex = "#if defined(HL_VECTOR)\n\
                  uniform vec4 value;\n\
                  #else\n\
                  uniform float value;\n\
                  #endif\n\
                  void main(){ gl_Position = vec4(value); }\n";
    let (uniforms, _) = glsl::StageSources::new(vertex, "void main(){}")
        .uniform_layout()
        .expect("one arm is live");
    assert_eq!(
        uniforms
            .iter()
            .map(|uniform| (uniform.name.as_str(), uniform.ty.as_str()))
            .collect::<Vec<_>>(),
        [("value", "float")]
    );
}

#[test]
fn oversized_array_is_rejected_without_integer_wraparound() {
    let fs = "uniform float hostile[4294967295];\nvoid main(){}\n";
    let error = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect_err("array exceeds the advertised component limit");
    assert!(matches!(
        error,
        glsl::UniformError::StageComponents {
            stage: "fragment",
            ..
        }
    ));
}

#[test]
fn nonliteral_array_dimension_is_a_link_error() {
    // A `#define`d or `const int` dimension IS an integral constant expression (GLSL ES 1.00 §4.1.9) and is
    // folded — see `preprocess::macro_array_dimension_and_inactive_branch`. Only a genuinely non-constant
    // dimension is a link error.
    let fs = "uniform int count;\nuniform vec4 values[count];\nvoid main(){}\n";
    assert_eq!(
        glsl::StageSources::new("void main(){}", fs).uniform_layout(),
        Err(glsl::UniformError::NonLiteralArray("values".into()))
    );
}

#[test]
fn uniform_components_are_enforced_per_stage() {
    // Exactly the advertised ceiling must LINK — an advertised limit the linker refuses is the bug this
    // pair guards against. `MAX_UNIFORM_VECTORS` is 2048 vec4s / 8192 components per stage.
    let accepted = "uniform vec4 values[2048];\nvoid main(){ gl_FragColor = values[2047]; }\n";
    assert!(
        glsl::StageSources::new("void main(){}", accepted)
            .uniform_layout()
            .is_ok(),
        "the advertised uniform-vector limit must remain usable"
    );

    let rejected = "uniform vec4 values[2049];\nvoid main(){ gl_FragColor = values[2048]; }\n";
    assert!(matches!(
        glsl::StageSources::new("void main(){}", rejected).uniform_layout(),
        Err(glsl::UniformError::StageComponents {
            stage: "fragment",
            ..
        })
    ));
}

// ---------------------------------------------------------------------------------------------------
// Verbatim-path uniform-block binding injection (naga glsl-in requires layout(binding=X))
// ---------------------------------------------------------------------------------------------------

#[test]
fn bindingless_uniform_block_gets_binding_zero_injected() {
    // Chrome/ANGLE forward-verbatim shape: a uniform block declared WITHOUT layout(binding=).
    let fs = "#version 300 es\nprecision highp float;\nuniform Block { mat4 uMVP; vec2 uScale; } inst;\n\
              out vec4 c;\nvoid main(){ c = inst.uMVP[0] + vec4(inst.uScale, 0.0, 0.0); }\n";
    let out = glsl::Source::new(fs).inject_uniform_block_bindings();
    assert!(
        out.contains("layout(binding = 0) uniform Block"),
        "binding 0 injected:\n{out}"
    );
    // Body + instance name + members preserved verbatim.
    assert!(out.contains("} inst;"), "instance name preserved:\n{out}");
    assert!(out.contains("mat4 uMVP;"), "members preserved:\n{out}");
}

#[test]
fn bindingless_block_with_std140_preserves_qualifier() {
    // A block with a memory-layout qualifier but no binding: binding merges INTO the existing layout list,
    // std140 preserved.
    let vs = "#version 300 es\nlayout(std140) uniform Ubo { vec4 a; };\nvoid main(){ gl_Position = a; }\n";
    let out = glsl::Source::new(vs).inject_uniform_block_bindings();
    assert!(out.contains("std140"), "std140 preserved:\n{out}");
    assert!(out.contains("binding = 0"), "binding merged in:\n{out}");
    // Merged into ONE layout group (no second `layout(` before the block).
    assert_eq!(
        out.matches("layout(").count(),
        1,
        "single merged layout group:\n{out}"
    );
}

#[test]
fn already_bound_block_is_byte_identical() {
    // GskGpu/GTK4 shape: block ALREADY carries binding=0 — must stay byte-for-byte unchanged (no regression
    // to the currently-green gtk4 verbatim path).
    let fs = "#version 320 es\nlayout(std140, binding = 0) uniform PushConstants { mat4 mvp; vec2 scale; };\n\
              uniform sampler2D uTex;\nin vec2 vUV;\nout vec4 c;\nvoid main(){ c = texture(uTex, vUV) * mvp[0]; }\n";
    assert_eq!(
        glsl::Source::new(fs).inject_uniform_block_bindings(),
        fs,
        "already-bound block must be untouched"
    );
}

#[test]
fn combined_sampler_globals_are_not_touched() {
    // The host's split_global_samplers assigns sampler bindings; injecting here would double-qualify. A
    // shader with ONLY a sampler global (no block) is returned byte-identical.
    let fs = "#version 300 es\nprecision mediump float;\nuniform sampler2D uTex;\nin vec2 v;\nout vec4 c;\n\
              void main(){ c = texture(uTex, v); }\n";
    assert_eq!(
        glsl::Source::new(fs).inject_uniform_block_bindings(),
        fs,
        "sampler global must not be rewritten"
    );
}

#[test]
fn multiple_bindingless_blocks_get_sequential_bindings() {
    let vs = "#version 300 es\nuniform A { vec4 a; };\nlayout(std140) uniform B { vec4 b; };\n\
              void main(){ gl_Position = a + b; }\n";
    let out = glsl::Source::new(vs).inject_uniform_block_bindings();
    assert!(
        out.contains("layout(binding = 0) uniform A"),
        "first block binding 0:\n{out}"
    );
    assert!(
        out.contains("binding = 1"),
        "second block binding 1:\n{out}"
    );
}

#[test]
fn uniform_keyword_in_comment_does_not_trip_injection() {
    let fs = "#version 300 es\n// uniform Fake { vec4 x; }\nprecision highp float;\nout vec4 c;\n\
              void main(){ c = vec4(1.0); }\n";
    assert_eq!(
        glsl::Source::new(fs).inject_uniform_block_bindings(),
        fs,
        "commented block must be ignored"
    );
}
