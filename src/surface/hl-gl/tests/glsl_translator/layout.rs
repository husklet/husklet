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
fn unsupported_struct_uniform_is_rejected_instead_of_given_a_fake_scalar_layout() {
    let fs = "struct Material { vec4 color; };\nuniform Material material;\nvoid main(){}\n";
    let error = glsl::StageSources::new("void main(){}", fs)
        .uniform_layout()
        .expect_err("struct layout is not implemented");
    assert!(matches!(
        error,
        glsl::UniformError::UnsupportedType(ref ty) if ty == "Material"
    ));
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
fn conflicting_conditional_uniform_declarations_are_rejected() {
    let vertex = "#if defined(HL_VECTOR)\n\
                  uniform vec4 value;\n\
                  #else\n\
                  uniform float value;\n\
                  #endif\n\
                  void main(){ gl_Position = vec4(value); }\n";
    assert_eq!(
        glsl::StageSources::new(vertex, "void main(){}").uniform_layout(),
        Err(glsl::UniformError::ConflictingDeclaration("value".into()))
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
    let fs = "#define COUNT 4\nuniform vec4 values[COUNT];\nvoid main(){}\n";
    assert_eq!(
        glsl::StageSources::new("void main(){}", fs).uniform_layout(),
        Err(glsl::UniformError::NonLiteralArray("values".into()))
    );
}

#[test]
fn uniform_components_are_enforced_per_stage() {
    let accepted = "uniform vec4 values[256];\nvoid main(){ gl_FragColor = values[255]; }\n";
    assert!(
        glsl::StageSources::new("void main(){}", accepted)
            .uniform_layout()
            .is_ok(),
        "the advertised 256-vector limit must remain usable"
    );

    let rejected = "uniform vec4 values[257];\nvoid main(){ gl_FragColor = values[256]; }\n";
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
