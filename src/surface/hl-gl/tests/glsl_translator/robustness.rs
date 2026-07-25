use super::*;

#[test]
fn empty_sources_produce_structurally_valid_stubs() {
    let (v, f) = glsl::StageSources::new("", "").translate_render();
    // No attributes/uniforms, but a valid pinned-version stage with an (empty) entry — not a panic.
    assert!(v.contains("#version 460"));
    assert!(v.contains("void main()"));
    // The fragment stub still synthesizes its required output.
    assert!(
        f.contains("layout(location = 0) out vec4 hl_FragColor;"),
        "{f}"
    );
}

#[test]
fn shader_without_main_yields_empty_body_not_a_crash() {
    // Declarations but no main(): reflection still works; the emitted stage has an empty main body.
    let vs = "attribute vec2 aPos;\nvarying vec2 vUV;\n";
    let fs = "uniform sampler2D uTex;\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(
        v.contains("layout(location = 0) in vec2 aPos;"),
        "decls still reflected: {v}"
    );
    assert!(v.contains("void main()"), "{v}");
    assert!(
        f.contains("layout(binding = 1) uniform texture2D uTex_hltex;"),
        "{f}"
    );
    assert!(
        f.contains("layout(binding = 2) uniform sampler uTex_hlsmp;"),
        "{f}"
    );
    assert_eq!(glsl::Source::new(vs).vertex_attrs().len(), 1);
}

#[test]
fn attribute_and_uniform_caps_are_enforced() {
    // 20 attributes declared, but the model caps the vertex-attribute count at 16.
    let mut vs = String::new();
    for i in 0..20 {
        vs.push_str(&format!("attribute vec4 a{i};\n"));
    }
    vs.push_str("void main(){ gl_Position = a0; }\n");
    let attrs = glsl::Source::new(&vs).vertex_attrs();
    assert_eq!(attrs.len(), 16, "attribute count is capped at 16");

    // 20 data uniforms → capped at 16 in the block.
    let mut fs = String::from("");
    for i in 0..20 {
        fs.push_str(&format!("uniform float u{i};\n"));
    }
    fs.push_str("void main(){ gl_FragColor = vec4(u0); }\n");
    let (unis, _) = glsl::StageSources::new("void main(){}", &fs).uniform_layout();
    assert!(
        unis.len() <= 16,
        "data-uniform count capped at 16, got {}",
        unis.len()
    );
}
