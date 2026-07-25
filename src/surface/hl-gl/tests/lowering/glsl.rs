use super::*;

#[test]
fn glsl_translate_forwards_desktop_glsl_per_stage() {
    let (vs, fs) = glsl::StageSources::new(VS, FS).translate_render();

    // Vertex: a desktop `#version`, the attribute regenerated as a `layout(location) in`, the varying as a
    // `layout(location) out`, and the body (incl. gl_Position) carried through verbatim.
    assert!(vs.contains("#version 460"), "desktop version pinned");
    assert!(
        vs.contains("layout(location = 0) in vec2 aPos;"),
        "attribute -> desktop in: {vs}"
    );
    assert!(
        vs.contains("layout(location = 0) out vec2 vUV;"),
        "varying -> desktop out: {vs}"
    );
    assert!(vs.contains("gl_Position ="), "vertex body carried through");

    // Fragment: the varying as an `in`, the sampler SPLIT into a `texture2D` (binding 1) + `sampler`
    // (binding 2) — naga rejects a combined `uniform sampler2D` — a synthesized `out vec4`, ES
    // `gl_FragColor` rewritten onto it, and the ES `texture2D(` call lowered to a desktop `texture(` over
    // the `sampler2D(tex, samp)` constructor.
    assert!(fs.contains("#version 460"));
    assert!(
        fs.contains("layout(location = 0) in vec2 vUV;"),
        "varying -> desktop in: {fs}"
    );
    assert!(
        fs.contains("layout(binding = 1) uniform texture2D uTex_hltex;"),
        "sampler texture decl: {fs}"
    );
    assert!(
        fs.contains("layout(binding = 2) uniform sampler uTex_hlsmp;"),
        "sampler decl: {fs}"
    );
    assert!(
        fs.contains("out vec4 hl_FragColor;"),
        "synthesized fragment output: {fs}"
    );
    assert!(
        fs.contains("hl_FragColor = texture(sampler2D(uTex_hltex, uTex_hlsmp), vUV)"),
        "gl_FragColor + texture2D lowered: {fs}"
    );
    assert!(
        !fs.contains("gl_FragColor"),
        "the ES gl_FragColor builtin is gone: {fs}"
    );
    assert!(
        !fs.contains("texture2D("),
        "the ES texture2D( call is gone: {fs}"
    );
}

#[test]
fn glsl_collects_vertex_attrs_and_samplers() {
    let attrs = glsl::Source::new(VS).vertex_attrs();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "aPos");
    assert_eq!(attrs[0].ty, "vec2");
    assert_eq!(
        glsl::StageSources::new(VS, FS).samplers(),
        vec!["uTex".to_string()]
    );
}
