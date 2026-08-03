use super::*;

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
