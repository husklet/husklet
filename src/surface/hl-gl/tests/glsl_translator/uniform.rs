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
    let (unis, total) = glsl::StageSources::new(vs, fs).uniform_layout();
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
