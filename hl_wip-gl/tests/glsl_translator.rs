//! Adversarial coverage for the just-rewritten GLSL-ES → naga-acceptable desktop GLSL 460 translator
//! (`adapter/glsl.rs`). The host (naga `glsl-in`) owns the real compile, so we assert the STRUCTURAL
//! invariants of the emitted source (the form naga accepts) + the reflection helpers the pipeline keys on,
//! across varied real shaders and malformed input. Every assertion checks REAL translator output — never
//! "didn't crash".

use hl_gl::adapter::glsl;

// ---------------------------------------------------------------------------------------------------
// desktop-GLSL form invariants: no ES dialect leaks; #version pinned; entry present
// ---------------------------------------------------------------------------------------------------

/// A helper that fails loudly if any ES-dialect token survives into the emitted desktop GLSL.
fn assert_no_es_leaks(src: &str) {
    for banned in ["attribute ", "varying ", "gl_FragColor", "texture2D(", "#version 300", " es\n"] {
        assert!(!src.contains(banned), "ES dialect token {banned:?} leaked into emitted GLSL:\n{src}");
    }
    assert!(src.contains("#version 460"), "desktop #version not pinned:\n{src}");
    assert!(src.contains("void main()"), "entry point missing:\n{src}");
}

#[test]
fn forward_verbatim_gate_diverts_only_gskgpu_shaped_source() {
    // GskGpu-shaped: a helper taking a combined sampler parameter — `translate_render` cannot preserve it,
    // so the driver forwards it verbatim to the host ES route.
    let gsk_frag = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 vUV;\n\
                    layout(location=0) out vec4 c;\nvec4 fetch(sampler2D t, vec2 p){ return texture(t,p); }\n\
                    void main(){ c = fetch(uTex, vUV); }\n";
    assert!(glsl::is_forward_verbatim(gsk_frag), "sampler-parameter helper must forward verbatim");
    // GskGpu-shaped vertex: gl_VertexID vertex-pulling (no attributes).
    let gsk_vert = "#version 320 es\nout vec2 vUV;\nvoid main(){ vUV = vec2(float(gl_VertexID)); }\n";
    assert!(glsl::is_forward_verbatim(gsk_vert), "gl_VertexID must forward verbatim");

    // Simple ES2 (attribute/varying/gl_FragColor, sampler only as a global) must NOT divert — it stays on
    // `translate_render` (the ES route does not handle the ES2 dialect).
    let es2_vs = "attribute vec3 aPos;\nvarying vec2 vUV;\nvoid main(){ gl_Position = vec4(aPos,1.0); }\n";
    let es2_fs = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\n\
                  void main(){ gl_FragColor = texture2D(uTex, vUV); }\n";
    assert!(!glsl::is_forward_verbatim(es2_vs), "simple ES2 vertex must keep translate_render");
    assert!(!glsl::is_forward_verbatim(es2_fs), "simple ES2 fragment (global sampler) must keep translate_render");
    // The `sampler2D(t,s)` constructor is NOT a parameter — must not trip the gate.
    let ctor = "void main(){ vec4 c = texture(sampler2D(a_hltex, a_hlsmp), vec2(0.0)); }\n";
    assert!(!glsl::is_forward_verbatim(ctor), "sampler2D constructor must not be read as a parameter");
}

#[test]
fn es2_attribute_varying_fragcolor_shader_is_fully_desktopized() {
    let vs = "attribute vec3 aPos;\nattribute vec2 aUV;\nvarying vec2 vUV;\nuniform mat4 uMVP;\n\
              void main(){ vUV = aUV; gl_Position = uMVP * vec4(aPos, 1.0); }\n";
    let fs = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\n\
              void main(){ gl_FragColor = texture2D(uTex, vUV); }\n";
    let (v, f) = glsl::translate_render(vs, fs);

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
    assert!(v.contains("layout(std140, binding = 0) uniform HlUniforms {"), "{v}");
    assert!(v.contains("mat4 uMVP;"), "{v}");
    assert!(f.contains("layout(binding = 1) uniform texture2D uTex_hltex;"), "{f}");
    assert!(f.contains("layout(binding = 2) uniform sampler uTex_hlsmp;"), "{f}");
    // The synthesized fragment output + rewritten gl_FragColor/texture2D, with the sampler recombined via
    // the `sampler2D(tex, samp)` constructor naga accepts.
    assert!(f.contains("layout(location = 0) out vec4 hl_FragColor;"), "{f}");
    assert!(f.contains("hl_FragColor = texture(sampler2D(uTex_hltex, uTex_hlsmp), vUV)"), "{f}");
    // The vertex body carries through verbatim (built-in gl_Position preserved).
    assert!(v.contains("gl_Position ="), "{v}");
}

#[test]
fn es3_in_out_shader_with_explicit_frag_output_keeps_the_named_output() {
    let vs = "#version 300 es\nin vec3 aPos;\nout vec3 vColor;\n\
              void main(){ vColor = aPos; gl_Position = vec4(aPos, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nin vec3 vColor;\nout vec4 fragColor;\n\
              void main(){ fragColor = vec4(vColor, 1.0); }\n";
    let (v, f) = glsl::translate_render(vs, fs);
    assert_no_es_leaks(&v);
    assert_no_es_leaks(&f);

    // ES3 `in` attribute recognized (no `attribute` keyword) and located.
    assert!(v.contains("layout(location = 0) in vec3 aPos;"), "{v}");
    // The named ES3 fragment output is REUSED (not replaced with hl_FragColor), at location 0.
    assert!(f.contains("layout(location = 0) out vec4 fragColor;"), "{f}");
    assert!(!f.contains("hl_FragColor"), "a named frag output must not synthesize hl_FragColor: {f}");
    // The frag body already writes `fragColor`; no gl_FragColor rewrite is needed and none is present.
    assert!(f.contains("fragColor = vec4(vColor, 1.0)"), "{f}");
}

#[test]
fn es_precision_qualifiers_are_stripped_from_the_carried_body() {
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    // A precision qualifier used INSIDE the body (not just the global `precision` statement).
    let fs = "precision mediump float;\nvoid main(){ highp float v = 0.5; gl_FragColor = vec4(v); }\n";
    let (_, f) = glsl::translate_render(vs, fs);
    // Desktop core GLSL rejects `highp`/`mediump`/`lowp` as qualifiers — they must be gone from the body.
    assert!(!f.contains("highp"), "highp not stripped from body: {f}");
    assert!(!f.contains("mediump"), "mediump not stripped: {f}");
    // The actual statement survives (minus the qualifier).
    assert!(f.contains("float v = 0.5;"), "body statement lost: {f}");
}

#[test]
fn comments_do_not_leak_phantom_attributes_or_uniforms() {
    // A commented-out legacy attribute + a block comment hiding a uniform must NOT be reflected.
    let vs = "// attribute vec4 aLegacy;\n/* uniform mat4 uOld; */\nattribute vec2 aPos;\n\
              void main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let (v, _) = glsl::translate_render(vs, fs);
    assert!(!v.contains("aLegacy"), "commented attribute leaked: {v}");
    assert!(!v.contains("uOld"), "commented uniform leaked: {v}");
    // aPos is the SOLE attribute and lands at location 0.
    assert!(v.contains("layout(location = 0) in vec2 aPos;"), "{v}");
    let attrs = glsl::collect_vertex_attrs(vs);
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "aPos");
}

#[test]
fn multiple_varyings_and_outs_get_distinct_sequential_locations() {
    let vs = "attribute vec3 aPos;\nvarying vec2 vUV;\nvarying vec3 vNormal;\nout vec4 vExtra;\n\
              void main(){ vUV = aPos.xy; vNormal = aPos; vExtra = vec4(1.0); gl_Position = vec4(aPos,1.0); }\n";
    let fs = "varying vec2 vUV;\nvarying vec3 vNormal;\nvoid main(){ gl_FragColor = vec4(vUV, vNormal.z, 1.0); }\n";
    let (v, f) = glsl::translate_render(vs, fs);
    assert!(v.contains("layout(location = 0) out vec2 vUV;"), "{v}");
    assert!(v.contains("layout(location = 1) out vec3 vNormal;"), "{v}");
    assert!(v.contains("layout(location = 2) out vec4 vExtra;"), "{v}");
    // The fragment stage receives the varyings it consumes at the same locations.
    assert!(f.contains("layout(location = 0) in vec2 vUV;"), "{f}");
    assert!(f.contains("layout(location = 1) in vec3 vNormal;"), "{f}");
}

#[test]
fn multiple_samplers_get_distinct_texture_and_sampler_bindings() {
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n";
    let fs = "varying vec2 vUV;\nuniform sampler2D uAlbedo;\nuniform sampler2D uNormal;\n\
              void main(){ gl_FragColor = texture2D(uAlbedo, vUV) + texture2D(uNormal, vUV); }\n";
    let (_, f) = glsl::translate_render(vs, fs);
    // With no UBO, sampler k owns texture binding 1+2k and sampler binding 2+2k — all four distinct.
    assert!(f.contains("layout(binding = 1) uniform texture2D uAlbedo_hltex;"), "{f}");
    assert!(f.contains("layout(binding = 2) uniform sampler uAlbedo_hlsmp;"), "{f}");
    assert!(f.contains("layout(binding = 3) uniform texture2D uNormal_hltex;"), "{f}");
    assert!(f.contains("layout(binding = 4) uniform sampler uNormal_hlsmp;"), "{f}");
    // The ES `texture2D(` CALLS are lowered to `texture(` (the `texture2D` type keyword in the decls is
    // fine); each sampler use is recombined via the `sampler2D(tex, samp)` constructor.
    assert!(!f.contains("texture2D("), "texture2D( call not lowered: {f}");
    assert!(f.contains("texture(sampler2D(uAlbedo_hltex, uAlbedo_hlsmp), vUV)"), "{f}");
    assert!(f.contains("texture(sampler2D(uNormal_hltex, uNormal_hlsmp), vUV)"), "{f}");
    assert_eq!(glsl::program_samplers(vs, fs), vec!["uAlbedo".to_string(), "uNormal".to_string()]);
}

#[test]
fn global_consts_are_hoisted_into_both_stages() {
    let vs = "const float PI = 3.14159;\nattribute vec2 aPos;\n\
              void main(){ gl_Position = vec4(aPos * PI, 0.0, 1.0); }\n";
    let fs = "const vec3 TINT = vec3(1.0, 0.5, 0.25);\nvoid main(){ gl_FragColor = vec4(TINT, 1.0); }\n";
    let (v, f) = glsl::translate_render(vs, fs);
    // A const declared in either stage is emitted into BOTH stages (deduped), before main.
    assert!(v.contains("const float PI = 3.14159;"), "{v}");
    assert!(v.contains("const vec3 TINT = vec3(1.0, 0.5, 0.25);"), "vs const hoist: {v}");
    assert!(f.contains("const float PI = 3.14159;"), "fs const hoist: {f}");
    assert!(f.contains("const vec3 TINT = vec3(1.0, 0.5, 0.25);"), "{f}");
}

// ---------------------------------------------------------------------------------------------------
// uniform-block std140 byte layout (uni_layout) — the offsets glUniform* + the UBO upload key on
// ---------------------------------------------------------------------------------------------------

#[test]
fn uni_layout_computes_std140_style_offsets_and_padded_total() {
    let vs = "uniform float uScale;\nuniform vec3 uColor;\nuniform mat4 uMVP;\nattribute vec2 aPos;\n\
              void main(){ gl_Position = uMVP * vec4(aPos, 0.0, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let (unis, total) = glsl::uni_layout(vs, fs);
    assert_eq!(unis.len(), 3);
    // float @0 sz4; vec3 aligns to 16 → @16 sz16; mat4 aligns to 16 → @32 sz64.
    assert_eq!((unis[0].name.as_str(), unis[0].off, unis[0].sz), ("uScale", 0, 4));
    assert_eq!((unis[1].name.as_str(), unis[1].off, unis[1].sz), ("uColor", 16, 16));
    assert_eq!((unis[2].name.as_str(), unis[2].off, unis[2].sz), ("uMVP", 32, 64));
    // Total rounds up to a 16-byte multiple: 32 + 64 = 96.
    assert_eq!(total, 96);
}

#[test]
fn uni_layout_separates_data_uniforms_from_samplers() {
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n";
    let fs = "uniform vec4 uTint;\nuniform sampler2D uTex;\nuniform samplerCube uEnv;\n\
              void main(){ gl_FragColor = uTint; }\n";
    // Samplers do NOT occupy uniform-block bytes.
    let (unis, total) = glsl::uni_layout(vs, fs);
    assert_eq!(unis.len(), 1, "only the data uniform is in the block");
    assert_eq!(unis[0].name, "uTint");
    assert_eq!(total, 16);
    // program_uniform_decls = data only; sampler decls carry the sampler types for glGetActiveUniform.
    let data = glsl::program_uniform_decls(vs, fs);
    assert_eq!(data.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["uTint"]);
    let samps = glsl::program_sampler_decls(vs, fs);
    assert_eq!(samps.iter().map(|d| (d.name.as_str(), d.ty.as_str())).collect::<Vec<_>>(),
        vec![("uTex", "sampler2D"), ("uEnv", "samplerCube")]);
}

#[test]
fn uniform_interface_block_members_are_enumerated() {
    // A named std140 interface block: its MEMBERS become the data uniforms.
    let vs = "uniform Matrices { mat4 uModel; mat4 uView; } mats;\nattribute vec3 aPos;\n\
              void main(){ gl_Position = uModel * uView * vec4(aPos, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let (unis, _total) = glsl::uni_layout(vs, fs);
    let names: Vec<_> = unis.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(names, vec!["uModel", "uView"], "block members enumerated as uniforms: {names:?}");
}

// ---------------------------------------------------------------------------------------------------
// fragment outputs reflection (glGetFragDataLocation / GL_PROGRAM_OUTPUT)
// ---------------------------------------------------------------------------------------------------

#[test]
fn frag_outputs_reflect_named_es3_outputs_and_none_for_es2() {
    let es3 = "out vec4 color0;\nout vec4 color1;\nvoid main(){ color0 = vec4(1.0); color1 = vec4(0.0); }\n";
    let outs = glsl::program_frag_outputs(es3);
    assert_eq!(outs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["color0", "color1"]);
    // An ES2 gl_FragColor shader declares NO named output.
    let es2 = "void main(){ gl_FragColor = vec4(1.0); }\n";
    assert!(glsl::program_frag_outputs(es2).is_empty());
}

// ---------------------------------------------------------------------------------------------------
// compute translation (translate_compute)
// ---------------------------------------------------------------------------------------------------

#[test]
fn translate_compute_pins_desktop_version_and_strips_es_dialect() {
    let cs = "#version 310 es\nlayout(local_size_x = 64) in;\n\
              layout(std430, binding = 0) buffer Data { float v[]; };\n\
              void main(){ highp uint i = gl_GlobalInvocationID.x; v[i] = float(i); }\n";
    let out = glsl::translate_compute(cs);
    assert!(out.starts_with("#version 460\n"), "compute version pinned: {out}");
    assert!(!out.contains("#version 310"), "ES compute version not stripped: {out}");
    assert!(!out.contains("highp"), "ES precision not stripped from compute: {out}");
    // The compute body + SSBO layout survive.
    assert!(out.contains("layout(local_size_x = 64) in;"), "{out}");
    assert!(out.contains("void main()"), "{out}");
}

// ---------------------------------------------------------------------------------------------------
// malformed / degenerate input — honest structural output, never a panic or silent-wrong
// ---------------------------------------------------------------------------------------------------

#[test]
fn empty_sources_produce_structurally_valid_stubs() {
    let (v, f) = glsl::translate_render("", "");
    // No attributes/uniforms, but a valid pinned-version stage with an (empty) entry — not a panic.
    assert!(v.contains("#version 460"));
    assert!(v.contains("void main()"));
    // The fragment stub still synthesizes its required output.
    assert!(f.contains("layout(location = 0) out vec4 hl_FragColor;"), "{f}");
}

#[test]
fn shader_without_main_yields_empty_body_not_a_crash() {
    // Declarations but no main(): reflection still works; the emitted stage has an empty main body.
    let vs = "attribute vec2 aPos;\nvarying vec2 vUV;\n";
    let fs = "uniform sampler2D uTex;\n";
    let (v, f) = glsl::translate_render(vs, fs);
    assert!(v.contains("layout(location = 0) in vec2 aPos;"), "decls still reflected: {v}");
    assert!(v.contains("void main()"), "{v}");
    assert!(f.contains("layout(binding = 1) uniform texture2D uTex_hltex;"), "{f}");
    assert!(f.contains("layout(binding = 2) uniform sampler uTex_hlsmp;"), "{f}");
    assert_eq!(glsl::collect_vertex_attrs(vs).len(), 1);
}

#[test]
fn attribute_and_uniform_caps_are_enforced() {
    // 20 attributes declared, but the model caps the vertex-attribute count at 16.
    let mut vs = String::new();
    for i in 0..20 {
        vs.push_str(&format!("attribute vec4 a{i};\n"));
    }
    vs.push_str("void main(){ gl_Position = a0; }\n");
    let attrs = glsl::collect_vertex_attrs(&vs);
    assert_eq!(attrs.len(), 16, "attribute count is capped at 16");

    // 20 data uniforms → capped at 16 in the block.
    let mut fs = String::from("");
    for i in 0..20 {
        fs.push_str(&format!("uniform float u{i};\n"));
    }
    fs.push_str("void main(){ gl_FragColor = vec4(u0); }\n");
    let (unis, _) = glsl::uni_layout("void main(){}", &fs);
    assert!(unis.len() <= 16, "data-uniform count capped at 16, got {}", unis.len());
}

#[test]
fn int_and_matrix_uniform_types_layout_without_panicking() {
    // Exercise the full type table in uni_layout (ivec/uvec/matNxM) — must produce monotonic offsets.
    let fs = "uniform int uA;\nuniform ivec2 uB;\nuniform uvec3 uC;\nuniform mat3 uD;\nuniform mat2 uE;\n\
              void main(){ gl_FragColor = vec4(float(uA)); }\n";
    let (unis, total) = glsl::uni_layout("void main(){}", fs);
    assert_eq!(unis.len(), 5);
    // Offsets are non-decreasing and each member fits before the next.
    for w in unis.windows(2) {
        assert!(w[1].off >= w[0].off + w[0].sz, "overlapping members: {:?}", unis);
    }
    assert!(total >= unis.last().unwrap().off + unis.last().unwrap().sz);
    assert_eq!(total % 16, 0, "block total is 16-byte aligned");
}

// ---------------------------------------------------------------------------------------------------
// Verbatim-path uniform-block binding injection (naga glsl-in requires layout(binding=X))
// ---------------------------------------------------------------------------------------------------

#[test]
fn bindingless_uniform_block_gets_binding_zero_injected() {
    // Chrome/ANGLE forward-verbatim shape: a uniform block declared WITHOUT layout(binding=).
    let fs = "#version 300 es\nprecision highp float;\nuniform Block { mat4 uMVP; vec2 uScale; } inst;\n\
              out vec4 c;\nvoid main(){ c = inst.uMVP[0] + vec4(inst.uScale, 0.0, 0.0); }\n";
    let out = glsl::inject_uniform_block_bindings(fs);
    assert!(out.contains("layout(binding = 0) uniform Block"), "binding 0 injected:\n{out}");
    // Body + instance name + members preserved verbatim.
    assert!(out.contains("} inst;"), "instance name preserved:\n{out}");
    assert!(out.contains("mat4 uMVP;"), "members preserved:\n{out}");
}

#[test]
fn bindingless_block_with_std140_preserves_qualifier() {
    // A block with a memory-layout qualifier but no binding: binding merges INTO the existing layout list,
    // std140 preserved.
    let vs = "#version 300 es\nlayout(std140) uniform Ubo { vec4 a; };\nvoid main(){ gl_Position = a; }\n";
    let out = glsl::inject_uniform_block_bindings(vs);
    assert!(out.contains("std140"), "std140 preserved:\n{out}");
    assert!(out.contains("binding = 0"), "binding merged in:\n{out}");
    // Merged into ONE layout group (no second `layout(` before the block).
    assert_eq!(out.matches("layout(").count(), 1, "single merged layout group:\n{out}");
}

#[test]
fn already_bound_block_is_byte_identical() {
    // GskGpu/GTK4 shape: block ALREADY carries binding=0 — must stay byte-for-byte unchanged (no regression
    // to the currently-green gtk4 verbatim path).
    let fs = "#version 320 es\nlayout(std140, binding = 0) uniform PushConstants { mat4 mvp; vec2 scale; };\n\
              uniform sampler2D uTex;\nin vec2 vUV;\nout vec4 c;\nvoid main(){ c = texture(uTex, vUV) * mvp[0]; }\n";
    assert_eq!(glsl::inject_uniform_block_bindings(fs), fs, "already-bound block must be untouched");
}

#[test]
fn combined_sampler_globals_are_not_touched() {
    // The host's split_global_samplers assigns sampler bindings; injecting here would double-qualify. A
    // shader with ONLY a sampler global (no block) is returned byte-identical.
    let fs = "#version 300 es\nprecision mediump float;\nuniform sampler2D uTex;\nin vec2 v;\nout vec4 c;\n\
              void main(){ c = texture(uTex, v); }\n";
    assert_eq!(glsl::inject_uniform_block_bindings(fs), fs, "sampler global must not be rewritten");
}

#[test]
fn multiple_bindingless_blocks_get_sequential_bindings() {
    let vs = "#version 300 es\nuniform A { vec4 a; };\nlayout(std140) uniform B { vec4 b; };\n\
              void main(){ gl_Position = a + b; }\n";
    let out = glsl::inject_uniform_block_bindings(vs);
    assert!(out.contains("layout(binding = 0) uniform A"), "first block binding 0:\n{out}");
    assert!(out.contains("binding = 1"), "second block binding 1:\n{out}");
}

#[test]
fn uniform_keyword_in_comment_does_not_trip_injection() {
    let fs = "#version 300 es\n// uniform Fake { vec4 x; }\nprecision highp float;\nout vec4 c;\n\
              void main(){ c = vec4(1.0); }\n";
    assert_eq!(glsl::inject_uniform_block_bindings(fs), fs, "commented block must be ignored");
}

#[test]
fn skia_bare_default_block_uniforms_are_wrapped_into_binding_zero() {
    // The real Chrome/Skia GPU-raster shape: BARE default-block uniforms + gl_VertexID (→ verbatim path).
    // naga rejects the implicit default uniform block; wrap into a single binding-0 std140 block.
    let vs = "#version 300 es\nprecision mediump float;\nuniform highp vec4 sk_RTAdjust;\n\
              uniform highp vec2 uatlas;\nin highp vec4 fillBounds;\nout highp vec2 vAtlas;\n\
              void main(){ vAtlas = fillBounds.xy * uatlas; gl_Position = vec4(fillBounds.xy * sk_RTAdjust.xz, 0.0, 1.0) + float(gl_VertexID); }\n";
    let combined = glsl::program_uniform_decls(vs, "void main(){}");
    let out = glsl::prepare_verbatim_stage(vs, &combined);
    // The bare declarations are gone; a single binding-0 std140 block carries both members.
    assert!(!out.contains("uniform highp vec4 sk_RTAdjust;"), "bare uniform removed:\n{out}");
    assert!(out.contains("layout(std140, binding = 0) uniform HlUniforms"), "wrapped block present:\n{out}");
    assert!(out.contains("vec4 sk_RTAdjust;") && out.contains("vec2 uatlas;"), "members carried:\n{out}");
    // Exactly one binding-0 UBO (no double-declaration), body references preserved by plain name.
    assert_eq!(out.matches("binding = 0").count(), 1, "single binding-0 block:\n{out}");
    assert!(out.contains("gl_VertexID"), "body preserved:\n{out}");
}

#[test]
fn skia_wrap_uses_combined_cross_stage_layout() {
    // vs declares A, fs declares B — both bare. Each stage's wrapped block must carry the COMBINED [A,B]
    // set so the std140 offsets agree with uni_layout(vs,fs) / Program::ubuf.
    let vs = "#version 300 es\nuniform vec4 uA;\nvoid main(){ gl_Position = uA + float(gl_VertexID); }\n";
    let fs = "#version 300 es\nprecision mediump float;\nuniform vec4 uB;\nout vec4 c;\n\
              void main(){ c = uB; }\n";
    let combined = glsl::program_uniform_decls(vs, fs);
    let vout = glsl::prepare_verbatim_stage(vs, &combined);
    let fout = glsl::prepare_verbatim_stage(fs, &combined);
    for (stage, out) in [("vs", &vout), ("fs", &fout)] {
        assert!(out.contains("vec4 uA;") && out.contains("vec4 uB;"), "{stage} carries combined set:\n{out}");
        assert!(out.contains("binding = 0"), "{stage} block bound:\n{out}");
    }
    // fs's bare uB is removed and replaced by the combined block (member order matches uni_layout).
    assert!(!fout.contains("uniform vec4 uB;"), "fs bare uniform removed:\n{fout}");
}

#[test]
fn gskgpu_block_style_program_is_untouched_by_verbatim_prep() {
    // GskGpu keeps uniforms in an explicit bound block — no bare data uniforms, so no wrapping, and the
    // block already has binding=0 → byte-identical (no gtk4 regression).
    let vs = "#version 320 es\nlayout(std140, binding = 0) uniform PushConstants { mat4 mvp; vec2 scale; };\n\
              void main(){ gl_Position = mvp * vec4(scale, 0.0, 1.0) + float(gl_VertexID); }\n";
    let fs = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 v;\nout vec4 c;\n\
              void main(){ c = texture(uTex, v); }\n";
    let combined = glsl::program_uniform_decls(vs, fs);
    assert_eq!(glsl::prepare_verbatim_stage(vs, &combined), vs, "gskgpu vs must be byte-identical");
    assert_eq!(glsl::prepare_verbatim_stage(fs, &combined), fs, "gskgpu fs must be byte-identical");
}
