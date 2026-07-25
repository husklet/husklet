use super::*;

#[test]
fn global_consts_are_hoisted_into_both_stages() {
    let vs = "const float PI = 3.14159;\nattribute vec2 aPos;\n\
              void main(){ gl_Position = vec4(aPos * PI, 0.0, 1.0); }\n";
    let fs =
        "const vec3 TINT = vec3(1.0, 0.5, 0.25);\nvoid main(){ gl_FragColor = vec4(TINT, 1.0); }\n";
    let (v, f) = glsl::StageSources::new(vs, fs).translate_render();
    // A const declared in either stage is emitted into BOTH stages (deduped), before main.
    assert!(v.contains("const float PI = 3.14159;"), "{v}");
    assert!(
        v.contains("const vec3 TINT = vec3(1.0, 0.5, 0.25);"),
        "vs const hoist: {v}"
    );
    assert!(
        f.contains("const float PI = 3.14159;"),
        "fs const hoist: {f}"
    );
    assert!(f.contains("const vec3 TINT = vec3(1.0, 0.5, 0.25);"), "{f}");
}

// ---------------------------------------------------------------------------------------------------
// uniform-block std140 byte layout (uni_layout) — the offsets glUniform* + the UBO upload key on
// ---------------------------------------------------------------------------------------------------

#[test]
fn uni_layout_computes_std140_style_offsets_and_padded_total() {
    let vs =
        "uniform float uScale;\nuniform vec3 uColor;\nuniform mat4 uMVP;\nattribute vec2 aPos;\n\
              void main(){ gl_Position = uMVP * vec4(aPos, 0.0, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let (unis, total) = glsl::StageSources::new(vs, fs).uniform_layout();
    assert_eq!(unis.len(), 3);
    // float @0 sz4; vec3 aligns to 16 → @16 sz16; mat4 aligns to 16 → @32 sz64.
    assert_eq!(
        (unis[0].name.as_str(), unis[0].off, unis[0].sz),
        ("uScale", 0, 4)
    );
    assert_eq!(
        (unis[1].name.as_str(), unis[1].off, unis[1].sz),
        ("uColor", 16, 16)
    );
    assert_eq!(
        (unis[2].name.as_str(), unis[2].off, unis[2].sz),
        ("uMVP", 32, 64)
    );
    // Total rounds up to a 16-byte multiple: 32 + 64 = 96.
    assert_eq!(total, 96);
}

#[test]
fn uni_layout_separates_data_uniforms_from_samplers() {
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n";
    let fs = "uniform vec4 uTint;\nuniform sampler2D uTex;\nuniform samplerCube uEnv;\n\
              void main(){ gl_FragColor = uTint; }\n";
    // Samplers do NOT occupy uniform-block bytes.
    let (unis, total) = glsl::StageSources::new(vs, fs).uniform_layout();
    assert_eq!(unis.len(), 1, "only the data uniform is in the block");
    assert_eq!(unis[0].name, "uTint");
    assert_eq!(total, 16);
    // program_uniform_decls = data only; sampler decls carry the sampler types for glGetActiveUniform.
    let data = glsl::StageSources::new(vs, fs).uniform_decls();
    assert_eq!(
        data.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        vec!["uTint"]
    );
    let samps = glsl::StageSources::new(vs, fs).sampler_decls();
    assert_eq!(
        samps
            .iter()
            .map(|d| (d.name.as_str(), d.ty.as_str()))
            .collect::<Vec<_>>(),
        vec![("uTex", "sampler2D"), ("uEnv", "samplerCube")]
    );
}

#[test]
fn uniform_interface_block_members_are_enumerated() {
    // A named std140 interface block: its MEMBERS become the data uniforms.
    let vs = "uniform Matrices { mat4 uModel; mat4 uView; } mats;\nattribute vec3 aPos;\n\
              void main(){ gl_Position = uModel * uView * vec4(aPos, 1.0); }\n";
    let fs = "void main(){ gl_FragColor = vec4(1.0); }\n";
    let (unis, _total) = glsl::StageSources::new(vs, fs).uniform_layout();
    let names: Vec<_> = unis.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["uModel", "uView"],
        "block members enumerated as uniforms: {names:?}"
    );
}

// ---------------------------------------------------------------------------------------------------
// fragment outputs reflection (glGetFragDataLocation / GL_PROGRAM_OUTPUT)
// ---------------------------------------------------------------------------------------------------

#[test]
fn frag_outputs_reflect_named_es3_outputs_and_none_for_es2() {
    let es3 = "out vec4 color0;\nout vec4 color1;\nvoid main(){ color0 = vec4(1.0); color1 = vec4(0.0); }\n";
    let outs = glsl::StageSources::new("", es3).frag_outputs();
    assert_eq!(
        outs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        vec!["color0", "color1"]
    );
    // An ES2 gl_FragColor shader declares NO named output.
    let es2 = "void main(){ gl_FragColor = vec4(1.0); }\n";
    assert!(glsl::StageSources::new("", es2).frag_outputs().is_empty());
}

// ---------------------------------------------------------------------------------------------------
// compute translation (translate_compute)
// ---------------------------------------------------------------------------------------------------
