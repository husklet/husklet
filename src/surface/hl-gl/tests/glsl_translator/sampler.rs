use super::*;

#[test]
fn multiple_samplers_get_distinct_texture_and_sampler_bindings() {
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n";
    let fs = "varying vec2 vUV;\nuniform sampler2D uAlbedo;\nuniform sampler2D uNormal;\n\
              void main(){ gl_FragColor = texture2D(uAlbedo, vUV) + texture2D(uNormal, vUV); }\n";
    let (_, f) = glsl::StageSources::new(vs, fs).translate_render();
    // With no UBO, sampler k owns texture binding 1+2k and sampler binding 2+2k — all four distinct.
    assert!(
        f.contains("layout(binding = 1) uniform texture2D uAlbedo_hltex;"),
        "{f}"
    );
    assert!(
        f.contains("layout(binding = 2) uniform sampler uAlbedo_hlsmp;"),
        "{f}"
    );
    assert!(
        f.contains("layout(binding = 3) uniform texture2D uNormal_hltex;"),
        "{f}"
    );
    assert!(
        f.contains("layout(binding = 4) uniform sampler uNormal_hlsmp;"),
        "{f}"
    );
    // The ES `texture2D(` CALLS are lowered to `texture(` (the `texture2D` type keyword in the decls is
    // fine); each sampler use is recombined via the `sampler2D(tex, samp)` constructor.
    assert!(
        !f.contains("texture2D("),
        "texture2D( call not lowered: {f}"
    );
    assert!(
        f.contains("texture(sampler2D(uAlbedo_hltex, uAlbedo_hlsmp), vUV)"),
        "{f}"
    );
    assert!(
        f.contains("texture(sampler2D(uNormal_hltex, uNormal_hlsmp), vUV)"),
        "{f}"
    );
    assert_eq!(
        glsl::StageSources::new(vs, fs).samplers(),
        vec!["uAlbedo".to_string(), "uNormal".to_string()]
    );
}
