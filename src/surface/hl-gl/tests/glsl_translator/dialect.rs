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
fn es3_in_out_shader_with_explicit_frag_output_keeps_the_named_output() {
    let vs = "#version 300 es\nin vec3 aPos;\nout vec3 vColor;\n\
              void main(){ vColor = aPos; gl_Position = vec4(aPos, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nin vec3 vColor;\nout vec4 fragColor;\n\
              void main(){ fragColor = vec4(vColor, 1.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert_no_es_leaks(&v);
    assert_no_es_leaks(&f);

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
