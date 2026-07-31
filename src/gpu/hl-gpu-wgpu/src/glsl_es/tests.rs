use super::*;

#[test]
fn detects_es_and_combined_sampler_but_not_desktop() {
    assert!(Source::new("#version 320 es\nvoid main(){}").is_es());
    assert!(Source::new("#version 300 es\nuniform sampler2D t;").is_es());
    assert!(Source::new("uniform sampler2D t; void main(){}").is_es());
    assert!(Source::new("void main(){ int i = gl_VertexID; }").is_es());
    // Already-desktop split source (what the ES2 driver path emits) must NOT be re-taken.
    let desktop = "#version 460\nlayout(binding=1) uniform texture2D t_hltex;\nlayout(binding=2) uniform sampler t_hlsmp;\nvoid main(){ vec4 c = texture(sampler2D(t_hltex,t_hlsmp), vec2(0.0)); }";
    assert!(
        !Source::new(desktop).is_es(),
        "desktop split source must keep the direct path"
    );
}

#[test]
fn splits_global_sampler_and_recombines_at_builtin() {
    let src = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 uv;\nout vec4 c;\nvoid main(){ c = texture(uTex, uv); }";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(out.contains("#version 460"), "{out}");
    assert!(!out.contains("320 es"), "{out}");
    assert!(!out.contains("precision"), "precision stripped: {out}");
    assert!(out.contains("uniform texture2D uTex_hltex"), "{out}");
    assert!(out.contains("uniform sampler uTex_hlsmp"), "{out}");
    assert!(
        out.contains("texture(sampler2D(uTex_hltex, uTex_hlsmp), uv)"),
        "{out}"
    );
}

#[test]
fn splits_sampler_function_parameter_and_call_site() {
    let src = "#version 320 es\nuniform sampler2D uTex;\nvec4 fetch(sampler2D tex, vec2 p){ return texture(tex, p); }\nvoid main(){ gl_Position = fetch(uTex, vec2(0.0)); }";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    // parameter split into two
    assert!(
        out.contains("texture2D tex_hltex, sampler tex_hlsmp"),
        "param split: {out}"
    );
    // recombine inside helper (texture builtin)
    assert!(
        out.contains("texture(sampler2D(tex_hltex, tex_hlsmp), p)"),
        "helper recombine: {out}"
    );
    // pass split pair at the user-function call site
    assert!(
        out.contains("fetch(uTex_hltex, uTex_hlsmp, vec2(0.0))"),
        "call-site pair: {out}"
    );
}

// --- The REAL GskGpu constructs (verbatim shapes from the GTK4 GskGpu source our shim forwards) -------

#[test]
fn seeds_version_define_so_gsk_ubo_binding_branch_wins() {
    // GskGpu gates its UBO binding on `__VERSION__`, which naga's preprocessor leaves undefined (= 0),
    // so the pinned `#version 460` alone would still pick the no-binding branch. We inject the define.
    let out = Source::new("#version 320 es\n#define GSK_GLES 1\nvoid main(){}\n")
        .normalize(naga::ShaderStage::Vertex);
    assert!(out.contains("#version 460"), "version pinned: {out}");
    assert!(
        out.contains("#define __VERSION__ 460"),
        "__VERSION__ seeded: {out}"
    );
    assert!(!out.contains("320 es"), "es version gone: {out}");
}

#[test]
fn rewrites_gl_vertexid_hidden_in_gsk_vertex_index_macro() {
    // The exact GskGpu form: the builtin lives only inside the macro *body*.
    let src = "#version 320 es\n#define GSK_VERTEX_INDEX gl_VertexID\nvoid main(){ int i = int(GSK_VERTEX_INDEX); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("#define GSK_VERTEX_INDEX int(gl_VertexIndex)"),
        "macro body rewritten: {out}"
    );
    assert!(
        !out.contains("gl_VertexID"),
        "no raw gl_VertexID survives: {out}"
    );
}

#[test]
fn adds_explicit_location_to_gsk_io_macros() {
    let src = "#version 320 es\n#define IN(_loc) in\n#define PASS(_loc) out\n#define PASS_FLAT(_loc) flat in\nvoid main(){}\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("#define IN(_loc) layout(location = _loc) in"),
        "IN: {out}"
    );
    assert!(
        out.contains("#define PASS(_loc) layout(location = _loc) out"),
        "PASS: {out}"
    );
    assert!(
        out.contains("#define PASS_FLAT(_loc) layout(location = _loc) flat in"),
        "PASS_FLAT: {out}"
    );
}

#[test]
fn rewrites_std140_mat2_member_to_vec4_columns_and_reconstructs_uses() {
    // The ANGLE mat2-in-UBO shape naga-24 rejects. The member becomes `vec4 m2__col[2]` (identical
    // std140 bytes) and each use is reconstructed with the column-vector constructor.
    let src = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat2 m2; } x;\nlayout(location = 0) in vec2 aPos;\nvoid main(){ gl_Position = vec4(x.m2 * aPos, 0.0, 1.0); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("vec4 m2__col[2];"),
        "member rewritten to vec4 col array: {out}"
    );
    assert!(!out.contains("mat2 m2"), "original mat2 member gone: {out}");
    assert!(
        out.contains("mat2(x.m2__col[0].xy, x.m2__col[1].xy)"),
        "use reconstructed: {out}"
    );
    assert!(!out.contains("x.m2 "), "raw block.m2 use gone: {out}");
}

#[test]
fn rewrites_std140_mat3x2_and_mat4x2_with_right_column_counts() {
    let src = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat3x2 a; mat4x2 b; } x;\nvoid main(){ vec2 p = x.a * vec3(1.0) + x.b * vec4(1.0); gl_Position = vec4(p, 0.0, 1.0); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("vec4 a__col[3];"),
        "mat3x2 -> 3 columns: {out}"
    );
    assert!(
        out.contains("vec4 b__col[4];"),
        "mat4x2 -> 4 columns: {out}"
    );
    assert!(
        out.contains("mat3x2(x.a__col[0].xy, x.a__col[1].xy, x.a__col[2].xy)"),
        "mat3x2 recon: {out}"
    );
    assert!(
        out.contains("mat4x2(x.b__col[0].xy, x.b__col[1].xy, x.b__col[2].xy, x.b__col[3].xy)"),
        "mat4x2 recon: {out}"
    );
}

#[test]
fn std140_mat2_pass_leaves_mat3_mat4_and_nonblock_mat2_untouched() {
    // 3-/4-row matrices in a std140 block are accepted by naga and must NOT be reshaped.
    let block = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat3 m3; mat4 m4; } x;\nvoid main(){ gl_Position = x.m4 * vec4(x.m3 * vec3(1.0), 1.0); }\n";
    let out = Source::new(block).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("mat3 m3;") && out.contains("mat4 m4;"),
        "mat3/mat4 members untouched: {out}"
    );
    assert!(
        !out.contains("__col"),
        "no column rewrite for 3/4-row matrices: {out}"
    );
    // A non-block (plain global) mat2 already validates and must be left alone.
    let plain = "#version 300 es\nuniform mat2 uRot;\nlayout(location=0) in vec2 aPos;\nvoid main(){ gl_Position = vec4(uRot * aPos, 0.0, 1.0); }\n";
    let out = Source::new(plain).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("uniform mat2 uRot;"),
        "plain uniform mat2 untouched: {out}"
    );
    assert!(
        !out.contains("__col"),
        "no column rewrite for plain mat2: {out}"
    );
}

#[test]
fn lowers_returning_switch_to_if_else_chain() {
    // A GskGpu color-state style switch: returning cases, stacked labels, and a `default`.
    let src = "#version 320 es\nint apply(uint cs){\n  switch (cs)\n    {\n    case 0u:\n      return 10;\n    case 1u:\n    case 2u:\n      return 20;\n    default:\n      return 0;\n    }\n}\nvoid main(){}\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(!out.contains("switch"), "switch removed: {out}");
    assert!(!out.contains("case "), "case labels removed: {out}");
    assert!(out.contains("if ("), "if branch present: {out}");
    assert!(out.contains("else if ("), "else-if branch present: {out}");
    assert!(out.contains("else {"), "default became else: {out}");
    // Stacked labels 1u/2u OR into one condition.
    assert!(
        out.contains("== (1u)") && out.contains("== (2u)") && out.contains("||"),
        "stacked labels OR'd: {out}"
    );
}

/// The GL driver collects a program's bare default-block uniforms into an ANONYMOUS
/// `layout(std140, binding = 0) uniform HlUniforms { … };`, whose members live in global scope and are
/// referenced by their plain name. That form was parsed and then silently skipped, so a `mat2` member
/// reached naga intact and was rejected (`UnsupportedMatrixTypeInStd140`) — which wedged the context and
/// made every later shader in the process fail to compile. The rewrite must cover it, reconstructing the
/// matrix at the BARE use site.
#[test]
fn rewrites_anonymous_std140_block_and_reconstructs_bare_uses() {
    let src = "#version 300 es\nlayout(std140, binding = 0) uniform HlUniforms { mat2 a; };\nlayout(location=0) in vec2 aPos;\nvoid main(){ gl_Position = vec4(a * aPos, 0.0, 1.0); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(out.contains("vec4 a__col[2]"), "member split: {out}");
    assert!(!out.contains("mat2 a;"), "original mat2 member gone: {out}");
    assert!(
        out.contains("mat2(a__col[0].xy, a__col[1].xy)"),
        "bare use reconstructed without an instance qualifier: {out}"
    );
}

#[test]
fn rewrites_anonymous_std140_mat3x2_and_mat4x2() {
    let src = "#version 300 es\nlayout(std140, binding = 0) uniform HlUniforms { mat3x2 a; mat4x2 b; };\nvoid main(){ vec2 p = a * vec3(1.0) + b * vec4(1.0); gl_Position = vec4(p, 0.0, 1.0); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(out.contains("vec4 a__col[3]"), "mat3x2 → 3 columns: {out}");
    assert!(out.contains("vec4 b__col[4]"), "mat4x2 → 4 columns: {out}");
    assert!(
        out.contains("mat3x2(a__col[0].xy, a__col[1].xy, a__col[2].xy)"),
        "mat3x2 use: {out}"
    );
    assert!(
        out.contains("mat4x2(b__col[0].xy, b__col[1].xy, b__col[2].xy, b__col[3].xy)"),
        "mat4x2 use: {out}"
    );
}

/// An anonymous block's members are bare globals, so a local of the same name SHADOWS the member. This
/// pass has no scope tracking, so it must decline the rewrite for such a name rather than silently
/// redirect the local's reads to the uniform. Declining leaves naga to reject the shader loudly.
#[test]
fn declines_anonymous_rewrite_when_a_local_shadows_the_member() {
    let src = "#version 300 es\nlayout(std140, binding = 0) uniform HlUniforms { mat2 a; };\nlayout(location=0) in vec2 aPos;\nvoid main(){ mat2 a = mat2(1.0); gl_Position = vec4(a * aPos, 0.0, 1.0); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(
        !out.contains("__col"),
        "a shadowed member must not be rewritten: {out}"
    );
    assert!(out.contains("mat2 a;"), "member left declared: {out}");
}

/// A named-instance block keeps working exactly as before — the instance qualifies the use, so no
/// shadowing question arises.
#[test]
fn named_instance_block_is_unaffected_by_the_anonymous_support() {
    let src = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat2 m2; } x;\nlayout(location=0) in vec2 aPos;\nvoid main(){ mat2 m2 = mat2(1.0); gl_Position = vec4(x.m2 * aPos * m2, 0.0, 1.0); }\n";
    let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
    assert!(
        out.contains("mat2(x.m2__col[0].xy, x.m2__col[1].xy)"),
        "qualified use still rewritten despite a same-named local: {out}"
    );
}

/// naga's std140 2-row-matrix restriction is a LAYOUT rule, not an ES one. The GL driver's ES2 path
/// rewrites its shaders to desktop form before they arrive, so `is_es()` is false and `normalize` never
/// runs — which is why a plain `uniform mat2` (collected into the driver's default block) still failed
/// after the anonymous-block gate was widened. The pass must therefore be reachable on both routes.
#[test]
fn desktop_route_shaders_also_get_the_std140_mat2_rewrite() {
    let desktop = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { mat2 a; };\nlayout(location=0) in vec2 aPos;\nvoid main(){ gl_Position = vec4(a * aPos, 0.0, 1.0); }\n";
    assert!(
        !Source::new(desktop).is_es(),
        "the driver's translated output is not ES-shaped"
    );
    assert!(
        crate::wgsl::glsl_to_wgsl(desktop, naga::ShaderStage::Vertex, "main").is_ok(),
        "a desktop-route std140 mat2 must compile"
    );
    // And a shader with no std140 mat2 is returned byte-for-byte by the unconditional pass.
    let plain = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { vec4 a; };\nvoid main(){ gl_Position = a; }\n";
    assert_eq!(
        Source::new(plain).split_std140_mat2(),
        plain,
        "byte-faithful when there is nothing to rewrite"
    );
}

// ---------------------------------------------------------------------------------------------------
// std140 arrays of scalars / 2-component vectors
// ---------------------------------------------------------------------------------------------------

/// `uniform float u[4]` reaches naga intact and is typed `array<f32, 4>` — stride 4 — which wgpu's
/// validator refuses in the uniform address space ("array stride 4 is not a multiple of the required
/// alignment 16"). std140 requires the same 16-byte stride, and the driver already writes it that way, so
/// the member is declared as an array of `vec4` and the value swizzled back at each use.
#[test]
fn pads_scalar_std140_array_to_vec4_and_swizzles_uses() {
    let src = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { float u[4]; };\nlayout(location=0) out vec4 c;\nvoid main(){ c = vec4(u[2]); }\n";
    let out = Source::new(src).pad_std140_arrays();
    assert!(out.contains("vec4 u__arr[4]"), "member padded: {out}");
    assert!(!out.contains("float u[4]"), "original member gone: {out}");
    assert!(out.contains("u__arr[2].x"), "use swizzled: {out}");
}

/// `int`/`uint` arrays pad to the MATCHING vector type (a `vec4` would change the element's type), and a
/// `vec2` array recovers two components.
#[test]
fn pads_integer_and_vec2_std140_arrays_to_their_own_vector_types() {
    let src = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { int k[2]; uint m[2]; vec2 v[2]; };\nlayout(location=0) out vec4 c;\nvoid main(){ c = vec4(float(k[0]) + float(m[1]), v[1].x, 0.0, 0.0); }\n";
    let out = Source::new(src).pad_std140_arrays();
    assert!(out.contains("ivec4 k__arr[2]"), "int → ivec4: {out}");
    assert!(out.contains("uvec4 m__arr[2]"), "uint → uvec4: {out}");
    assert!(out.contains("vec4 v__arr[2]"), "vec2 → vec4: {out}");
    assert!(out.contains("k__arr[0].x"), "int use: {out}");
    assert!(out.contains("m__arr[1].x"), "uint use: {out}");
    assert!(out.contains("v__arr[1].xy"), "vec2 use recovers two components: {out}");
}

/// An element type whose array stride is ALREADY 16 must be left completely alone — padding it would
/// change nothing and only risk a mistranslation.
#[test]
fn leaves_already_aligned_std140_members_byte_for_byte() {
    for src in [
        "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { vec4 u[4]; };\nvoid main(){ gl_Position = u[1]; }\n",
        "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { vec3 u[4]; };\nvoid main(){ gl_Position = vec4(u[1], 1.0); }\n",
        "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { mat4 u[2]; };\nvoid main(){ gl_Position = u[1][0]; }\n",
        "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { float a; vec2 b; };\nvoid main(){ gl_Position = vec4(a, b, 1.0); }\n",
    ] {
        assert_eq!(
            Source::new(src).pad_std140_arrays(),
            src,
            "byte-faithful when there is nothing to rewrite"
        );
    }
}

/// A named-instance block's uses are `x.u[i]`; only the member name is rewritten, so the qualifier and the
/// index expression survive untouched.
#[test]
fn pads_named_instance_std140_arrays_and_keeps_the_index_expression() {
    let src = "#version 460\nlayout(std140, binding = 0) uniform Xf { float u[4]; int k; } x;\nlayout(location=0) out vec4 c;\nvoid main(){ c = vec4(x.u[x.k + 1]); }\n";
    let out = Source::new(src).pad_std140_arrays();
    assert!(out.contains("vec4 u__arr[4]"), "member padded: {out}");
    assert!(
        out.contains("x.u__arr[x.k + 1].x"),
        "qualified use keeps its qualifier and index expression: {out}"
    );
}

/// An anonymous block's members are bare globals, so a local of the same name SHADOWS the member and this
/// pass has no scope tracking. Decline the rewrite rather than redirect the local's reads to the uniform —
/// wgpu then refuses the module loudly, which beats a silently wrong value.
#[test]
fn declines_padding_when_a_local_shadows_the_array_member() {
    let src = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { float u[4]; };\nlayout(location=0) out vec4 c;\nvoid main(){ float u[4]; u[0] = 1.0; c = vec4(u[0]); }\n";
    assert_eq!(
        Source::new(src).pad_std140_arrays(),
        src,
        "a shadowed member must not be rewritten"
    );
}

/// A use that is not an element subscript — passing the whole array, or `.length()` — has no swizzled
/// equivalent this textual pass can write, so the member is declined entirely rather than half-rewritten.
#[test]
fn declines_padding_when_the_array_is_used_whole() {
    let src = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { float u[4]; };\nlayout(location=0) out vec4 c;\nfloat s(float a[4]){ return a[0]; }\nvoid main(){ c = vec4(s(u)); }\n";
    assert_eq!(
        Source::new(src).pad_std140_arrays(),
        src,
        "a non-subscript use must decline the rewrite"
    );
}

/// The stride rule is a LAYOUT rule, not an ES one: the GL driver rewrites its ES2 shaders to desktop form
/// before they arrive, so `is_es()` is false and `normalize` never runs. The pass must be reachable on
/// BOTH routes, exactly like the 2-row-matrix split beside it.
#[test]
fn both_dialect_routes_compile_a_std140_scalar_array() {
    let desktop = "#version 460\nlayout(std140, binding = 0) uniform HlUniforms { float u[4]; };\nlayout(location=0) out vec4 c;\nvoid main(){ c = vec4(u[2]); }\n";
    assert!(
        !Source::new(desktop).is_es(),
        "the driver's translated output is not ES-shaped"
    );
    assert!(
        crate::wgsl::glsl_to_wgsl(desktop, naga::ShaderStage::Fragment, "main").is_ok(),
        "a desktop-route std140 scalar array must compile"
    );
    let es = "#version 300 es\nprecision mediump float;\nlayout(std140, binding = 0) uniform HlUniforms { float u[4]; };\nlayout(location=0) out vec4 c;\nvoid main(){ c = vec4(u[2]); }\n";
    assert!(Source::new(es).is_es(), "the ES route is taken");
    assert!(
        crate::wgsl::glsl_to_wgsl(es, naga::ShaderStage::Fragment, "main").is_ok(),
        "an ES-route std140 scalar array must compile"
    );
}
