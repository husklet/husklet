use super::*;

#[test]
fn skia_bare_default_block_uniforms_are_wrapped_into_binding_zero() {
    // The real Chrome/Skia GPU-raster shape: BARE default-block uniforms + gl_VertexID (→ verbatim path).
    // naga rejects the implicit default uniform block; wrap into a single binding-0 std140 block.
    let vs = "#version 300 es\nprecision mediump float;\nuniform highp vec4 sk_RTAdjust;\n\
              uniform highp vec2 uatlas;\nin highp vec4 fillBounds;\nout highp vec2 vAtlas;\n\
              void main(){ vAtlas = fillBounds.xy * uatlas; gl_Position = vec4(fillBounds.xy * sk_RTAdjust.xz, 0.0, 1.0) + float(gl_VertexID); }\n";
    let combined = glsl::StageSources::new(vs, "void main(){}").uniform_decls();
    let out = glsl::Source::new(vs).prepare_verbatim_stage(&combined);
    // The bare declarations are gone; a single binding-0 std140 block carries both members.
    assert!(
        !out.contains("uniform highp vec4 sk_RTAdjust;"),
        "bare uniform removed:\n{out}"
    );
    assert!(
        out.contains("layout(std140, binding = 0) uniform HlUniforms"),
        "wrapped block present:\n{out}"
    );
    assert!(
        out.contains("vec4 sk_RTAdjust;") && out.contains("vec2 uatlas;"),
        "members carried:\n{out}"
    );
    // Exactly one binding-0 UBO (no double-declaration), body references preserved by plain name.
    assert_eq!(
        out.matches("binding = 0").count(),
        1,
        "single binding-0 block:\n{out}"
    );
    assert!(out.contains("gl_VertexID"), "body preserved:\n{out}");
}

#[test]
fn skia_wrap_uses_combined_cross_stage_layout() {
    // vs declares A, fs declares B — both bare. Each stage's wrapped block must carry the COMBINED [A,B]
    // set so the std140 offsets agree with uni_layout(vs,fs) / Program::ubuf.
    let vs = "#version 300 es\nuniform vec4 uA;\nvoid main(){ gl_Position = uA + float(gl_VertexID); }\n";
    let fs = "#version 300 es\nprecision mediump float;\nuniform vec4 uB;\nout vec4 c;\n\
              void main(){ c = uB; }\n";
    let combined = glsl::StageSources::new(vs, fs).uniform_decls();
    let vout = glsl::Source::new(vs).prepare_verbatim_stage(&combined);
    let fout = glsl::Source::new(fs).prepare_verbatim_stage(&combined);
    for (stage, out) in [("vs", &vout), ("fs", &fout)] {
        assert!(
            out.contains("vec4 uA;") && out.contains("vec4 uB;"),
            "{stage} carries combined set:\n{out}"
        );
        assert!(out.contains("binding = 0"), "{stage} block bound:\n{out}");
    }
    // fs's bare uB is removed and replaced by the combined block (member order matches uni_layout).
    assert!(
        !fout.contains("uniform vec4 uB;"),
        "fs bare uniform removed:\n{fout}"
    );
}

#[test]
fn translated_skia_program_preserves_and_compiles_its_seventeenth_uniform() {
    // Reduced from the Chrome shader that produced `UnknownVariable("ublend_S3")`. Vertex declarations
    // precede fragment declarations in the combined layout, and the matrix is shared across stages.
    // Neither stage contains a verbatim-route trigger: this exercises `translate_render`.
    let vs = "#version 300 es\n\
              uniform highp vec4 sk_RTAdjust;\n\
              uniform highp mat3 umatrix_S1_c0_c0_c1;\n\
              in highp vec2 position;\n\
              out highp vec2 coords;\n\
              void main(){ coords = position; gl_Position = sk_RTAdjust + \
                  vec4((umatrix_S1_c0_c0_c1 * vec3(position, 1.0)).xy, 0.0, 1.0); }\n";
    let fs = "#version 300 es\n\
              precision mediump float;\n\
              uniform highp vec2 u_skRTFlip;\n\
              uniform mediump vec4 uDstTextureCoords_S0;\n\
              uniform highp vec4 uthresholds_S1_c0_c0_c0[2];\n\
              uniform highp vec4 uscale_S1_c0_c0_c0[8];\n\
              uniform highp vec4 ubias_S1_c0_c0_c0[8];\n\
              uniform mediump float ubias_S1_c0_c0_c1_c0;\n\
              uniform mediump float uscale_S1_c0_c0_c1_c0;\n\
              uniform highp mat3 umatrix_S1_c0_c0_c1;\n\
              uniform mediump vec4 uleftBorderColor_S1_c0_c0;\n\
              uniform mediump vec4 urightBorderColor_S1_c0_c0;\n\
              uniform highp mat3 umatrix_S1_c1;\n\
              uniform mediump float urange_S1;\n\
              uniform highp vec4 uinnerRect_S2;\n\
              uniform mediump vec2 uscale_S2;\n\
              uniform highp vec2 uinvRadiiXY_S2;\n\
              uniform mediump vec4 ublend_S3;\n\
              in highp vec2 coords;\n\
              out mediump vec4 color;\n\
              void main(){ color = ublend_S3 + vec4(coords, u_skRTFlip.x, urange_S1); }\n";

    assert!(!glsl::Source::new(vs).is_forward_verbatim());
    assert!(!glsl::Source::new(fs).is_forward_verbatim());
    let combined = glsl::StageSources::new(vs, fs).uniform_decls();
    let (layout, _) = glsl::StageSources::new(vs, fs)
        .uniform_layout()
        .expect("supported uniform layout");
    let (vertex, fragment) = glsl::StageSources::new(vs, fs).translate_render();

    assert_eq!(
        combined.len(),
        17,
        "declaration reflection must not truncate"
    );
    assert_eq!(layout.len(), 17, "buffer layout must match declarations");
    assert!(
        fragment.contains("vec4 ublend_S3;"),
        "the rebuilt uniform block dropped a live declaration:\n{fragment}"
    );
    assert!(
        fragment.contains("ublend_S3 +"),
        "the shader use must remain intact:\n{fragment}"
    );
    assert_naga_parses(&vertex, naga::ShaderStage::Vertex);
    assert_naga_parses(&fragment, naga::ShaderStage::Fragment);
}

#[test]
fn skia_wrap_preserves_every_comma_separated_uniform() {
    // Chrome/Skia may group multiple default-block uniforms in one declaration. The wrapper removes the
    // whole declaration, so reflection must rebuild every declarator—not only the first one—or naga reports
    // `UnknownVariable` for a live trailing name such as the observed `ublend_S3`.
    let vs = "#version 300 es\n\
              uniform highp float uscale_S3, ubias_S3[2], ublend_S3;\n\
              void main(){ gl_Position = vec4(uscale_S3 + ubias_S3[1] + ublend_S3) \
                  + float(gl_VertexID); }\n";

    let combined = glsl::StageSources::new(vs, "void main(){}").uniform_decls();
    let (layout, _) = glsl::StageSources::new(vs, "void main(){}")
        .uniform_layout()
        .expect("supported uniform layout");
    let out = glsl::Source::new(vs).prepare_verbatim_stage(&combined);

    assert_eq!(
        combined
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>(),
        ["uscale_S3", "ubias_S3", "ublend_S3"],
        "reflection must preserve declaration order"
    );
    assert_eq!(layout.len(), 3, "buffer layout must cover every declarator");
    assert!(
        out.contains("float uscale_S3;")
            && out.contains("float ubias_S3[2];")
            && out.contains("float ublend_S3;"),
        "the rebuilt block dropped a comma-separated uniform:\n{out}"
    );
}

#[test]
fn conditional_first_uniform_cannot_hide_the_generated_block() {
    let vertex = "#version 460\n\
                  #if 0\n\
                  uniform float inactive;\n\
                  #endif\n\
                  uniform vec4 ublend_S3;\n\
                  void main(){ gl_Position = ublend_S3; }\n";
    let combined = glsl::StageSources::new(vertex, "").uniform_decls();
    let output = glsl::Source::new(vertex).prepare_verbatim_stage(&combined);

    let block = output.find("uniform HlUniforms").expect("generated block");
    let conditional = output.find("#if 0").expect("conditional preserved");
    assert!(
        block < conditional,
        "the generated block must remain outside the inactive branch:\n{output}"
    );
    assert_eq!(
        output.matches("vec4 ublend_S3;").count(),
        1,
        "only the unconditional block member remains:\n{output}"
    );
    assert_naga_parses(&output, naga::ShaderStage::Vertex);
}

#[test]
fn conditional_grouped_duplicates_survive_program_prep_and_naga() {
    let vertex = "#version 460\n\
                  #extension GL_ARB_separate_shader_objects : enable\n\
                  #define HL_SCALE 1.0\n\
                  #ifdef HL_UNUSED_BRANCH\n\
                  uniform vec4 branch_pad, branch_blend;\n\
                  #else\n\
                  uniform vec4 active_pad, ublend_S3;\n\
                  #endif\n\
                  in vec4 position;\n\
                  out vec4 color;\n\
                  void main(){ color = ublend_S3; gl_Position = position * HL_SCALE; }\n";
    let fragment = "#version 460\n\
                    #if 0\n\
                    uniform vec4 fragment_pad;\n\
                    #endif\n\
                    in vec4 color;\n\
                    out vec4 output_color;\n\
                    void main(){ output_color = color + ublend_S3; }\n";
    let combined = glsl::StageSources::new(vertex, fragment).uniform_decls();
    let (vertex, fragment) = glsl::prepare_verbatim_program(vertex, fragment, &combined);

    for (stage, source) in [("vertex", &vertex), ("fragment", &fragment)] {
        assert_eq!(
            source.matches("vec4 ublend_S3;").count(),
            1,
            "{stage} must carry one deduplicated unconditional member:\n{source}"
        );
        assert!(
            source.find("uniform HlUniforms").expect("generated block")
                < source
                    .find("#ifdef")
                    .or_else(|| source.find("#if"))
                    .unwrap(),
            "{stage} block inherited a conditional:\n{source}"
        );
    }
    assert_naga_parses(&vertex, naga::ShaderStage::Vertex);
    assert_naga_parses(&fragment, naga::ShaderStage::Fragment);
}

#[test]
fn gskgpu_block_style_program_is_untouched_by_verbatim_prep() {
    // GskGpu keeps uniforms in an explicit bound block — no bare data uniforms, so no wrapping, and the
    // block already has binding=0 → byte-identical (no gtk4 regression).
    let vs = "#version 320 es\nlayout(std140, binding = 0) uniform PushConstants { mat4 mvp; vec2 scale; };\n\
              void main(){ gl_Position = mvp * vec4(scale, 0.0, 1.0) + float(gl_VertexID); }\n";
    let fs = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 v;\nout vec4 c;\n\
              void main(){ c = texture(uTex, v); }\n";
    let combined = glsl::StageSources::new(vs, fs).uniform_decls();
    assert_eq!(
        glsl::Source::new(vs).prepare_verbatim_stage(&combined),
        vs,
        "gskgpu vs must be byte-identical"
    );
    assert_eq!(
        glsl::Source::new(fs).prepare_verbatim_stage(&combined),
        fs,
        "gskgpu fs must be byte-identical"
    );
}

#[test]
fn default_block_array_uniform_keeps_its_dimension() {
    // Skia's Gaussian-blur shape: a default-block uniform ARRAY indexed in a loop. translate_render must
    // emit the `[N]` dimension into HlUniforms (else naga sees a scalar indexed → InvalidStoreTypes).
    let vs = "#version 300 es\nin vec2 aPos;\nout vec2 vUV;\nvoid main(){ vUV = aPos; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nin vec2 vUV;\nuniform vec4 uKernel[8];\nout vec4 o;\n\
              void main(){ vec4 s = vec4(0.0); for (int i=0;i<8;++i){ s += uKernel[i]; } o = s; }\n";
    let (_v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(
        f.contains("vec4 uKernel[8];"),
        "array dimension preserved in block:\n{f}"
    );
    // std140 sizing: 8 * vec4 (16 B) = 128 B for the member.
    let (unis, total) = glsl::StageSources::new(vs, fs)
        .uniform_layout()
        .expect("supported uniform layout");
    let k = unis
        .iter()
        .find(|u| u.name == "uKernel")
        .expect("uKernel reflected");
    assert_eq!(k.sz, 128, "vec4[8] std140 size");
    assert!(total >= 128, "ubuf holds the array: {total}");
}

// ---------------------------------------------------------------------------------------------------
// verbatim path: layout(location=N) injection into BARE Skia-style in/out (Task #243)
// ---------------------------------------------------------------------------------------------------

#[test]
fn postfix_array_on_the_type_declares_the_same_uniform_array() {
    // GskGLRenderer's shape. `uniform vec4[3] x;` is `uniform vec4 x[3];` (GLSL ES 3.00 §4.1.9); the
    // scanner used to read `vec4[3]` as one type token and refuse the program with
    // `unsupported uniform type vec4[3]`, dropping GTK4 to GskNglRenderer.
    let vs = "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nuniform vec4[3] uColors;\nout vec4 o;\n\
              void main(){ o = uColors[0] + uColors[1] + uColors[2]; }\n";
    let (unis, total) = glsl::StageSources::new(vs, fs)
        .uniform_layout()
        .expect("postfix array uniform is supported");
    let c = unis
        .iter()
        .find(|u| u.name == "uColors")
        .expect("uColors reflected");
    assert_eq!(c.arr, 3, "three elements, not a scalar");
    assert_eq!(c.sz, 48, "vec4[3] std140 size");
    assert!(total >= 48, "ubuf holds the array: {total}");
    // The regenerated block must spell the dimension on the NAME, which is all naga accepts.
    let (_v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(
        f.contains("vec4 uColors[3];"),
        "dimension moved onto the declarator:\n{f}"
    );
    // GTK's real shader is macro-heavy ES and takes the VERBATIM route, which wraps the bare declaration
    // into `HlUniforms` — that block must carry the array too, or naga const-indexes a scalar.
    let combined = glsl::StageSources::new(vs, fs).uniform_decls();
    let wrapped = glsl::Source::new(fs).prepare_verbatim_stage(&combined);
    assert!(
        !wrapped.contains("uniform vec4[3] uColors;"),
        "bare postfix declaration removed:\n{wrapped}"
    );
    assert!(
        wrapped.contains("vec4 uColors[3];"),
        "wrapped block carries the dimension:\n{wrapped}"
    );
    // The point of the dimension: naga must see an array to const-index, not a scalar.
    assert_naga_parses(&f, naga::ShaderStage::Fragment);
}

#[test]
fn detached_postfix_array_and_block_member_postfix_array_both_parse() {
    let vs = "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    // `vec2 [4]` — the subscript as its own token — and a postfix array MEMBER of an interface block.
    let fs = "#version 300 es\nprecision highp float;\nuniform vec2 [4] uOffsets;\n\
              uniform Block { vec4[2] mColors; float mScale; };\nout vec4 o;\n\
              void main(){ o = vec4(uOffsets[0], 0.0, 1.0) + mColors[1] * mScale; }\n";
    let unis = glsl::StageSources::new(vs, fs)
        .uniform_layout()
        .expect("detached postfix array is supported")
        .0;
    let o = unis
        .iter()
        .find(|u| u.name == "uOffsets")
        .expect("uOffsets reflected");
    assert_eq!(o.arr, 4, "detached subscript belongs to the declarator");
    let blocks = glsl::StageSources::new(vs, fs).uniform_blocks();
    let member = blocks
        .iter()
        .flat_map(|b| &b.members)
        .find(|m| m.name == "mColors")
        .expect("block member reflected");
    assert_eq!(member.ty, "vec4", "type token has no subscript left on it");
    assert_eq!(member.arr, 2, "member dimension read from the type");
}

#[test]
fn an_array_of_arrays_uniform_is_refused_not_misparsed() {
    // GLSL ES has no arrays of arrays and this layout cannot model one. Refuse loudly rather than keep
    // whichever of the two dimensions was read last and lay out the wrong number of bytes.
    let vs = "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nuniform vec4[3] uGrid[2];\nout vec4 o;\n\
              void main(){ o = uGrid[0][0]; }\n";
    let error = glsl::StageSources::new(vs, fs)
        .uniform_layout()
        .expect_err("an array of arrays is not modelable");
    assert!(
        format!("{error}").contains("uGrid"),
        "the refusal names the declaration: {error}"
    );
}

/// Two blocks that both take the default binding are two blocks. The byte-assembly accessor dedupes by
/// binding point, which is right for feeding one bound range per point and wrong for reflection: GL
/// identifies an active uniform block by its interface name, and a program declaring `Matrices` and
/// `Material` with no explicit binding has two of them, not one.
#[test]
fn declared_blocks_are_identified_by_name_not_by_binding_point() {
    let vs = "#version 300 es\nlayout(std140) uniform Matrices { mat4 uMvp; };\n\
              layout(std140) uniform Material { vec4 uTint; };\n\
              void main(){ gl_Position = uMvp * uTint; }\n";
    let fs =
        "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o = vec4(1.0); }\n";

    let declared = glsl::StageSources::new(vs, fs).declared_uniform_blocks();
    assert_eq!(
        declared.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        ["Matrices", "Material"],
        "both blocks survive, in declaration order"
    );

    // The binding-keyed accessor keeps its own identity, which is what the byte assembly needs.
    let by_binding = glsl::StageSources::new(vs, fs).uniform_blocks();
    assert_eq!(
        by_binding.len(),
        1,
        "sharing a binding point makes them one range to assemble"
    );

    // A block declared in BOTH stages is one active block.
    let shared_vs = "#version 300 es\nlayout(std140) uniform Camera { mat4 uView; };\n\
                     void main(){ gl_Position = uView[0]; }\n";
    let shared_fs = "#version 300 es\nprecision highp float;\n\
                     layout(std140) uniform Camera { mat4 uView; };\nout vec4 o;\n\
                     void main(){ o = uView[1]; }\n";
    let shared = glsl::StageSources::new(shared_vs, shared_fs).declared_uniform_blocks();
    assert_eq!(
        shared.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        ["Camera"],
        "one block declared twice is one active block"
    );
}
