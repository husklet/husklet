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
