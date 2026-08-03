use super::*;

#[test]
fn forward_verbatim_gate_diverts_only_gskgpu_shaped_source() {
    // GskGpu-shaped: a helper taking a combined sampler parameter — `translate_render` cannot preserve it,
    // so the driver forwards it verbatim to the host ES route.
    let gsk_frag = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 vUV;\n\
                    layout(location=0) out vec4 c;\nvec4 fetch(sampler2D t, vec2 p){ return texture(t,p); }\n\
                    void main(){ c = fetch(uTex, vUV); }\n";
    assert!(
        glsl::Source::new(gsk_frag).is_forward_verbatim(),
        "sampler-parameter helper must forward verbatim"
    );
    // GskGpu-shaped vertex: gl_VertexID vertex-pulling (no attributes).
    let gsk_vert =
        "#version 320 es\nout vec2 vUV;\nvoid main(){ vUV = vec2(float(gl_VertexID)); }\n";
    assert!(
        glsl::Source::new(gsk_vert).is_forward_verbatim(),
        "gl_VertexID must forward verbatim"
    );

    // Simple ES2 (attribute/varying/gl_FragColor, sampler only as a global) must NOT divert — it stays on
    // `translate_render` (the ES route does not handle the ES2 dialect).
    let es2_vs =
        "attribute vec3 aPos;\nvarying vec2 vUV;\nvoid main(){ gl_Position = vec4(aPos,1.0); }\n";
    let es2_fs = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\n\
                  void main(){ gl_FragColor = texture2D(uTex, vUV); }\n";
    assert!(
        !glsl::Source::new(es2_vs).is_forward_verbatim(),
        "simple ES2 vertex must keep translate_render"
    );
    assert!(
        !glsl::Source::new(es2_fs).is_forward_verbatim(),
        "simple ES2 fragment (global sampler) must keep translate_render"
    );
    // The `sampler2D(t,s)` constructor is NOT a parameter — must not trip the gate.
    let ctor = "void main(){ vec4 c = texture(sampler2D(a_hltex, a_hlsmp), vec2(0.0)); }\n";
    assert!(
        !glsl::Source::new(ctor).is_forward_verbatim(),
        "sampler2D constructor must not be read as a parameter"
    );
}

#[test]
fn es2_attribute_varying_fragcolor_shader_is_fully_desktopized() {
    let vs = "attribute vec3 aPos;\nattribute vec2 aUV;\nvarying vec2 vUV;\nuniform mat4 uMVP;\n\
              void main(){ vUV = aUV; gl_Position = uMVP * vec4(aPos, 1.0); }\n";
    let fs = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\n\
              void main(){ gl_FragColor = texture2D(uTex, vUV); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();

    assert_no_es_leaks(&v);
    assert_no_es_leaks(&f);
    assert!(v.contains("#define HL_GLSL_ES100 1"), "{v}");
    assert!(f.contains("#define HL_GLSL_ES100 1"), "{f}");

    // Both attributes get sequential explicit locations in declaration order.
    assert!(v.contains("layout(location = 0) in vec3 aPos;"), "{v}");
    assert!(v.contains("layout(location = 1) in vec2 aUV;"), "{v}");
    // The varying is an `out` in the vertex stage and an `in` in the fragment stage at the SAME location.
    assert!(v.contains("layout(location = 0) out vec2 vUV;"), "{v}");
    assert!(f.contains("layout(location = 0) in vec2 vUV;"), "{f}");
    // The uniform block owns binding 0; the sole sampler is split into a texture2D (binding 1) + sampler
    // (binding 2) — naga rejects a combined `uniform sampler2D`, so distinct bindings + separated globals.
    assert!(
        v.contains("layout(std140, binding = 0) uniform HlUniforms {"),
        "{v}"
    );
    assert!(v.contains("mat4 uMVP;"), "{v}");
    assert!(
        f.contains("layout(binding = 1) uniform texture2D uTex_hltex;"),
        "{f}"
    );
    assert!(
        f.contains("layout(binding = 2) uniform sampler uTex_hlsmp;"),
        "{f}"
    );
    // The synthesized fragment output + rewritten gl_FragColor/texture2D, with the sampler recombined via
    // the `sampler2D(tex, samp)` constructor naga accepts.
    assert!(
        f.contains("layout(location = 0) out vec4 hl_FragColor;"),
        "{f}"
    );
    assert!(
        f.contains("hl_FragColor = texture(sampler2D(uTex_hltex, uTex_hlsmp), vUV)"),
        "{f}"
    );
    // The vertex body carries through verbatim (built-in gl_Position preserved).
    assert!(v.contains("gl_Position ="), "{v}");
}

#[test]
fn es2_fragdata_dynamic_zero_index_targets_the_single_color_output() {
    let vs = "attribute vec4 a_position; varying float v_index; void main(){ gl_Position=a_position; v_index=0.0; }";
    let fs = "varying mediump float v_index; void main(){ gl_FragData[int(v_index)] = vec4(0.0,1.0,0.0,1.0); }";
    let (_, translated) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(!translated.contains("gl_FragData"), "{translated}");
    assert!(translated.contains("hl_FragColor = vec4"), "{translated}");
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn es2_builtin_invariance_declaration_survives_translation() {
    let vs = "attribute highp vec4 a_input; invariant gl_Position; void main(){ gl_Position=a_input; }";
    let fs = "void main(){ gl_FragColor=vec4(1); }";
    let (translated, _) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(translated.contains("invariant gl_Position;"), "{translated}");
    assert_naga_parses(&translated, naga::ShaderStage::Vertex);
}

#[test]
fn es3_in_out_shader_with_explicit_frag_output_keeps_the_named_output() {
    let vs = "#version 300 es\nin vec3 aPos;\nout vec3 vColor;\n\
              void main(){ vColor = aPos; gl_Position = vec4(aPos, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nin vec3 vColor;\nout vec4 fragColor;\n\
              void main(){ fragColor = vec4(vColor, 1.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert_no_es_leaks(&v);
    assert_no_es_leaks(&f);
    assert!(!v.contains("HL_GLSL_ES100"), "{v}");
    assert!(!f.contains("HL_GLSL_ES100"), "{f}");

    // ES3 `in` attribute recognized (no `attribute` keyword) and located.
    assert!(v.contains("layout(location = 0) in vec3 aPos;"), "{v}");
    // The named ES3 fragment output is REUSED (not replaced with hl_FragColor), at location 0.
    assert!(
        f.contains("layout(location = 0) out vec4 fragColor;"),
        "{f}"
    );
    assert!(
        !f.contains("hl_FragColor"),
        "a named frag output must not synthesize hl_FragColor: {f}"
    );
    // The frag body already writes `fragColor`; no gl_FragColor rewrite is needed and none is present.
    assert!(f.contains("fragColor = vec4(vColor, 1.0)"), "{f}");
}

#[test]
fn es_precision_qualifiers_are_stripped_from_the_carried_body() {
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    // A precision qualifier used INSIDE the body (not just the global `precision` statement).
    let fs =
        "precision mediump float;\nvoid main(){ highp float v = 0.5; gl_FragColor = vec4(v); }\n";
    let (_, f) = glsl::StageSources::new(vs, fs).translate_render();
    // Desktop core GLSL rejects `highp`/`mediump`/`lowp` as qualifiers — they must be gone from the body.
    assert!(!f.contains("highp"), "highp not stripped from body: {f}");
    assert!(!f.contains("mediump"), "mediump not stripped: {f}");
    // The actual statement survives (minus the qualifier).
    assert!(f.contains("float v = 0.5;"), "body statement lost: {f}");
}

#[test]
fn comments_do_not_leak_phantom_attributes_or_uniforms() {
    // A commented-out attribute + a block comment hiding a uniform must NOT be reflected.
    let vs = "// attribute vec4 aLegacy;\n/* uniform mat4 uOld; */\nattribute vec2 aPos;\n\
              void main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let (v, _) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(!v.contains("aLegacy"), "commented attribute leaked: {v}");
    assert!(!v.contains("uOld"), "commented uniform leaked: {v}");
    // aPos is the SOLE attribute and lands at location 0.
    assert!(v.contains("layout(location = 0) in vec2 aPos;"), "{v}");
    let attrs = glsl::Source::new(vs).vertex_attrs();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "aPos");
}

#[test]
fn multiple_varyings_and_outs_get_distinct_sequential_locations() {
    let vs = "attribute vec3 aPos;\nvarying vec2 vUV;\nvarying vec3 vNormal;\nout vec4 vExtra;\n\
              void main(){ vUV = aPos.xy; vNormal = aPos; vExtra = vec4(1.0); gl_Position = vec4(aPos,1.0); }\n";
    let fs = "varying vec2 vUV;\nvarying vec3 vNormal;\nvoid main(){ gl_FragColor = vec4(vUV, vNormal.z, 1.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(v.contains("layout(location = 0) out vec2 vUV;"), "{v}");
    assert!(v.contains("layout(location = 1) out vec3 vNormal;"), "{v}");
    assert!(v.contains("layout(location = 2) out vec4 vExtra;"), "{v}");
    // The fragment stage receives the varyings it consumes at the same locations.
    assert!(f.contains("layout(location = 0) in vec2 vUV;"), "{f}");
    assert!(f.contains("layout(location = 1) in vec3 vNormal;"), "{f}");
}

// ---------------------------------------------------------------------------------------------------
// Integer samplers: declared as integer textures, and ACCESSED by fetch rather than by sampling
// ---------------------------------------------------------------------------------------------------

/// `usampler2D` was not in the sampler type set at all, so it was classified as a DATA uniform: it landed
/// in the uniform block, no texture binding was made for it, and the shader failed to compile. That is why
/// integer textures "sampled as zero" — nothing was sampling.
#[test]
fn an_integer_sampler_declares_an_integer_texture_and_a_sampler() {
    let fs = "#version 300 es\nprecision highp float;\nin vec2 uv;\n\
              uniform highp usampler2D tex;\nout vec4 c;\n\
              void main() { c = vec4(texture(tex, uv)) / 255.0; }\n";
    let out = fragment_of(fs);
    assert!(
        out.contains("uniform utexture2D tex_hltex"),
        "the texture global must carry the INTEGER texture type: {out}"
    );
    assert!(
        out.contains("tex_hlsmp"),
        "the sampler global is still declared — the driver binds a texture+sampler PAIR per sampler and \
         a bind group short of its layout is refused: {out}"
    );
    assert!(
        !out.contains("uniform highp usampler2D tex;"),
        "the combined declaration must not survive: {out}"
    );
}

/// An integer texture cannot be sampled — no normalized reading, so no filtering — and a bind group that
/// offers one to a sampling instruction is refused. `texelFetch` is the only legal access, and it indexes
/// by texel, so the normalized coordinate is scaled by `textureSize`.
#[test]
fn sampling_an_integer_sampler_becomes_a_texel_fetch() {
    let fs = "#version 300 es\nprecision highp float;\nin vec2 uv;\n\
              uniform highp usampler2D tex;\nout vec4 c;\n\
              void main() { c = vec4(texture(tex, uv)) / 255.0; }\n";
    let out = fragment_of(fs);
    assert!(
        out.contains("texelFetch(tex_hltex, ivec2((uv) * vec2(textureSize(tex_hltex, 0))), 0)"),
        "the sample must become an indexed fetch: {out}"
    );
    assert!(
        !out.contains("usampler2D(tex_hltex"),
        "the recombining constructor must NOT be emitted for an integer sampler: {out}"
    );
}

/// The coordinate is extracted paren-balanced, so an expression containing its own calls survives whole.
#[test]
fn an_integer_fetch_keeps_a_compound_coordinate_intact() {
    let fs = "#version 300 es\nprecision highp float;\nin vec2 uv;\n\
              uniform highp isampler2D tex;\nout vec4 c;\n\
              void main() { c = vec4(texture(tex, clamp(uv * 2.0, vec2(0.0), vec2(1.0)))); }\n";
    let out = fragment_of(fs);
    assert!(
        out.contains("(clamp(uv * 2.0, vec2(0.0), vec2(1.0))) * vec2(textureSize(tex_hltex, 0))"),
        "the whole coordinate expression must be carried through: {out}"
    );
    assert!(out.contains("uniform itexture2D tex_hltex"), "{out}");
}

/// An ORDINARY float sampler must be untouched by any of this.
#[test]
fn a_float_sampler_still_recombines_and_samples() {
    let fs = "#version 300 es\nprecision highp float;\nin vec2 uv;\n\
              uniform sampler2D tex;\nout vec4 c;\nvoid main() { c = texture(tex, uv); }\n";
    let out = fragment_of(fs);
    assert!(out.contains("uniform texture2D tex_hltex"), "{out}");
    assert!(out.contains("sampler2D(tex_hltex, tex_hlsmp)"), "{out}");
    assert!(!out.contains("texelFetch"), "no fetch rewrite on a float sampler: {out}");
}

/// Translate a fragment shader alongside a trivial vertex stage and return the emitted fragment source.
fn fragment_of(fs: &str) -> String {
    const VS: &str = "#version 300 es\nin vec2 position;\nout vec2 uv;\n\
                      void main() { uv = position; gl_Position = vec4(position, 0.0, 1.0); }\n";
    glsl::StageSources::new(VS, fs).translate_render().1
}

/// A guest may write `texelFetch`/`textureSize` on an integer sampler itself. Those uses want the bare
/// texture global — handing them the recombining constructor would give a fetch instruction a sampled
/// image, which the backend refuses.
#[test]
fn a_hand_written_integer_fetch_gets_the_bare_texture_global() {
    let fs = "#version 300 es\nprecision highp float;\n\
              uniform highp usampler2D tex;\nout vec4 c;\n\
              void main() { c = vec4(texelFetch(tex, ivec2(1, 2), 0)) / float(textureSize(tex, 0).x); }\n";
    let out = fragment_of(fs);
    assert!(
        out.contains("texelFetch(tex_hltex, ivec2(1, 2), 0)"),
        "the fetch keeps its own index and takes the bare global: {out}"
    );
    assert!(out.contains("textureSize(tex_hltex, 0)"), "{out}");
    assert!(
        !out.contains("usampler2D(tex_hltex"),
        "no constructor anywhere for an integer sampler: {out}"
    );
}

// ---------------------------------------------------------------------------------------------------
// A shader may use only what its declared #version defines
// ---------------------------------------------------------------------------------------------------

/// GLSL-ES §3.3. A 3.10 built-in under `#version 300 es` compiled here and failed on real hardware, so a
/// shader that only worked on Husklet shipped silently and the author found out from a device they do not
/// have. Matching is on CALL syntax, so a user-defined name is untouched.
#[test]
fn an_es_310_builtin_is_refused_below_its_version() {
    for (name, body) in [
        ("bitCount", "int n = bitCount(7);"),
        ("findMSB", "int n = findMSB(7);"),
        ("frexp", "int e; float m = frexp(1.5, e);"),
        ("textureGather", "vec4 g = textureGather(tex, uv);"),
        ("imageStore", "imageStore(img, ivec2(0), vec4(1.0));"),
    ] {
        let fs = format!(
            "#version 300 es\nprecision highp float;\nin vec2 uv;\nuniform sampler2D tex;\n\
             out vec4 c;\nvoid main() {{ {body} c = vec4(1.0); }}\n"
        );
        assert_eq!(
            glsl::builtin_above_declared_version(&fs),
            Some(name),
            "{name} is an ES 3.10 addition and this shader declares 300"
        );
    }
}

/// The same shader at 3.10 is legal and must compile — the rule is about the DECLARED version, not about
/// the built-in being unsupported.
#[test]
fn the_same_builtin_is_accepted_at_its_own_version() {
    let fs = "#version 310 es\nprecision highp float;\nout vec4 c;\n\
              void main() { c = vec4(float(bitCount(7))); }\n";
    assert_eq!(glsl::builtin_above_declared_version(fs), None);
}

/// A 3.00 shader that uses only 3.00 constructs is untouched — including the ones that merely LOOK newer.
/// `packSnorm2x16` and `texelFetch` are both 3.00 and must not be caught.
#[test]
fn ordinary_es_300_constructs_are_not_refused() {
    let fs = "#version 300 es\nprecision highp float;\nin vec2 uv;\nuniform sampler2D tex;\nout vec4 c;\n\
              void main() {\n\
                uint p = packSnorm2x16(vec2(0.25, 0.75));\n\
                vec4 t = texelFetch(tex, ivec2(0, 0), 0);\n\
                c = t + vec4(float(p));\n\
              }\n";
    assert_eq!(glsl::builtin_above_declared_version(fs), None);
}

/// A user-defined name that happens to match must not be caught: only a CALL counts, and a declaration
/// with the same spelling is the author's own function at their own version.
#[test]
fn a_user_defined_name_is_not_mistaken_for_the_builtin() {
    // Used as a variable, never called.
    let fs = "#version 300 es\nprecision highp float;\nout vec4 c;\n\
              void main() { float bitCount = 3.0; c = vec4(bitCount); }\n";
    assert_eq!(glsl::builtin_above_declared_version(fs), None);

    // A member access of that name is not the built-in either.
    let fs = "#version 300 es\nprecision highp float;\nout vec4 c;\nstruct S { float frexp; };\n\
              void main() { S s; s.frexp = 1.0; c = vec4(s.frexp); }\n";
    assert_eq!(glsl::builtin_above_declared_version(fs), None);
}

/// A shader with no `#version` is GLSL-ES 1.00, which is below 3.10 like any other.
#[test]
fn the_declared_version_defaults_to_one_hundred() {
    assert_eq!(glsl::declared_es_version("void main(){}\n"), 100);
    assert_eq!(glsl::declared_es_version("#version 300 es\n"), 300);
    assert_eq!(glsl::declared_es_version("  #version 310 es\n"), 310);
}
