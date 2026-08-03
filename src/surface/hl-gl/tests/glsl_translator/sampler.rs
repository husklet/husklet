use super::*;

#[test]
fn deqp_random_texture_sampler_name_does_not_capture_swizzle() {
    let vs = r#"attribute vec4 dEQP_Position;
attribute vec2 a_b;
attribute float a_d;
attribute vec4 a_i;
attribute vec4 a_j;
varying mediump vec2 b;
varying mediump float d;
varying mediump vec4 i;
varying mediump vec4 j;
void main() {
    gl_Position = dEQP_Position;
    b = a_b;
    d = a_d;
    i = a_i;
    j = a_j;
}"#;
    let fs = r#"precision mediump float;
uniform samplerCube a;
varying mediump vec2 b;
uniform mediump float c;
varying mediump float d;
const int f = ivec4(10.0, -2.25, -9.25, 0.5).qspt.t;
uniform sampler2D g;
varying mediump vec4 i;
varying mediump vec4 j;
float k = vec4(-10.0, true, -0.75, true).r - float(float(-1.75));
const bool l = true;
const float m = 0.75;
int n = (int(-7.0) - -8);
const int o = ((int(-7)));
void main() {
    gl_FragColor = vec4(b.s, int(m) * int(n), d * float(-1.75), int(c));
    gl_FragColor = gl_FragColor;
    k = k;
    k = (float(k) - -6.0);
    gl_FragColor = vec4(o, -0.125, 12, n) * vec4(m, l, -1.0, k) - gl_FragColor;
    gl_FragColor = j.rabg.abgr;
    vec4 h = i;
    gl_FragColor = h.xywz;
    vec4 e = texture2D(g, (b));
    e = vec4(c, f, bool(1), false).wxyz;
    gl_FragColor = textureCube(a, vec3(c, d, float(c)), b.g);
}"#;

    let (_, translated) = glsl::StageSources::new(vs, fs).translate_render();
    assert!(
        translated.contains(", b.g)"),
        "cube bias swizzle changed: {translated}"
    );
    assert!(
        !translated.contains("b.sampler2D"),
        "sampler name g must not replace the unrelated .g swizzle: {translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn one_letter_sampler_names_do_not_replace_swizzles() {
    let fs = "uniform sampler2D a;\nvoid main(){\n\
              vec4 value = vec4(1.0);\n\
              gl_FragColor = texture2D(a, vec2(0.0)) + vec4(value.a, value . a, 0.0, 0.0);\n}";
    let (_, translated) =
        glsl::StageSources::new("void main(){gl_Position=vec4(0.0);}", fs).translate_render();

    assert!(translated.contains("value.a"), "{translated}");
    assert!(translated.contains("value . a"), "{translated}");
    assert_eq!(
        translated
            .matches("sampler2D(a_hltex, a_hlsmp)")
            .count(),
        1,
        "{translated}"
    );
    assert_naga_parses(&translated, naga::ShaderStage::Fragment);
}

#[test]
fn layered_samplers_keep_dimension_across_sampling_size_and_lod() {
    let vs = "#version 300 es\nvoid main(){gl_Position=vec4(0);}";
    let fs = "#version 300 es\nprecision highp float;\n\
              uniform sampler3D volume;\n\
              uniform sampler2DArray layers;\n\
              out vec4 color;\n\
              void main(){\
                ivec3 size3d=textureSize(volume,0);\
                ivec3 size_array=textureSize(layers,0);\
                color=textureLod(volume,vec3(.5),0.0)\
                    +texture(layers,vec3(.5),0.0)\
                    +vec4(size3d+size_array,0);\
              }";
    let (_, output) = glsl::StageSources::new(vs, fs).translate_render();
    for expected in [
        "uniform texture3D volume_hltex;",
        "uniform texture2DArray layers_hltex;",
        "textureSize(sampler3D(volume_hltex, volume_hlsmp),0)",
        "textureSize(sampler2DArray(layers_hltex, layers_hlsmp),0)",
        "textureLod(sampler3D(volume_hltex, volume_hlsmp),vec3(.5),0.0)",
        "texture(sampler2DArray(layers_hltex, layers_hlsmp),vec3(.5),0.0)",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
    assert_naga_parses(&output, naga::ShaderStage::Fragment);
}

#[test]
fn the_advertised_per_stage_sampler_count_is_supported_and_one_more_is_rejected() {
    let mut accepted = String::new();
    for index in 0..16 {
        accepted.push_str(&format!("uniform sampler2D texture{index};\n"));
    }
    accepted.push_str("void main(){}\n");
    assert!(glsl::StageSources::new("void main(){}", &accepted)
        .uniform_layout()
        .is_ok());

    accepted.insert_str(0, "uniform sampler2D overflow;\n");
    assert!(matches!(
        glsl::StageSources::new("void main(){}", &accepted).uniform_layout(),
        Err(glsl::UniformError::Samplers(17))
    ));
}

#[test]
fn sampler_array_elements_consume_stage_and_combined_limits() {
    let accepted =
        "uniform sampler2D textures[16];\nvoid main(){gl_FragColor=texture2D(textures[15],vec2(0));}\n";
    assert!(glsl::StageSources::new("void main(){}", accepted)
        .uniform_layout()
        .is_ok());

    let fragment_overflow = accepted.replace("[16]", "[17]").replace("[15]", "[16]");
    assert!(matches!(
        glsl::StageSources::new("void main(){}", &fragment_overflow).uniform_layout(),
        Err(glsl::UniformError::Samplers(17))
    ));

    let vertex_overflow =
        "uniform sampler2D textures[17];\nvoid main(){gl_Position=texture2D(textures[16],vec2(0));}\n";
    assert!(matches!(
        glsl::StageSources::new(vertex_overflow, "void main(){}").uniform_layout(),
        Err(glsl::UniformError::Samplers(17))
    ));
}

#[test]
fn sampler_array_emits_one_binding_pair_per_element() {
    let vs = "attribute vec2 p;\nvoid main(){gl_Position=vec4(p,0,1);}";
    let fs = "uniform sampler2D images[2];\nvoid main(){\
              gl_FragColor=texture2D(images[0],vec2(0))+texture2D(images[1],vec2(0));}";
    let (_, output) = glsl::StageSources::new(vs, fs).translate_render();
    for expected in [
        "layout(binding = 1) uniform texture2D images_0_hltex;",
        "layout(binding = 2) uniform sampler images_0_hlsmp;",
        "layout(binding = 3) uniform texture2D images_1_hltex;",
        "layout(binding = 4) uniform sampler images_1_hlsmp;",
        "sampler2D(images_0_hltex, images_0_hlsmp)",
        "sampler2D(images_1_hltex, images_1_hlsmp)",
    ] {
        assert!(output.contains(expected), "missing {expected}:\n{output}");
    }
}

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
