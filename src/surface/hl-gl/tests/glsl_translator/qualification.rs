use super::assert_naga_parses;
use hl_gl::adapter::glsl;

fn stage_sources(parameter: &str) -> (String, String) {
    let vertex = format!(
        "precision mediump float; attribute highp vec4 dEQP_Position; \
         float foo0({parameter}) {{ return x + 1.0; }} \
         void main() {{ gl_Position = dEQP_Position + vec4(foo0(1.0) * 0.0); }}"
    );
    let fragment = "precision mediump float; void main() { gl_FragColor = vec4(1.0); }";
    glsl::StageSources::new(&vertex, fragment).translate_render()
}

#[test]
fn deqp_valid_parameter_qualifiers_reach_the_host_compiler() {
    for parameter in [
        "const in float x",
        "const in lowp float x",
        "const lowp float x",
    ] {
        let (vertex, _) = stage_sources(parameter);
        assert_naga_parses(&vertex, naga::ShaderStage::Vertex);
    }
}

#[test]
fn deqp_valid_varying_qualifiers_generate_a_matching_interface() {
    for declaration in [
        "invariant varying lowp float x0;",
        "invariant varying float x0;",
        "varying lowp float x0;",
    ] {
        let vertex = format!(
            "precision mediump float; attribute highp vec4 dEQP_Position; {declaration} \
             uniform mediump float x1; attribute mediump float x2; \
             void main() {{ x0 = 1.0; gl_Position = dEQP_Position; }}"
        );
        let fragment = format!(
            "precision mediump float; {declaration} uniform mediump float x1; \
             void main() {{ float result = x0 + x1; gl_FragColor = vec4(result); }}"
        );
        // Program::link's second translation pass supplies host locations for both the
        // live position and dEQP's deliberately-unused `x2` attribute. Reproduce that
        // exact route: vertex-input location 1 must not shift the varying interface.
        let bindings = std::collections::BTreeMap::from([
            ("dEQP_Position".to_string(), 0),
            ("x2".to_string(), 1),
        ]);
        let (vertex, fragment) =
            glsl::StageSources::new(&vertex, &fragment).translate_render_with(&bindings);
        assert_naga_parses(&vertex, naga::ShaderStage::Vertex);
        assert_naga_parses(&fragment, naga::ShaderStage::Fragment);
        assert!(
            vertex.contains("layout(location = 0) out float x0;"),
            "{vertex}"
        );
        assert!(
            fragment.contains("layout(location = 0) in float x0;"),
            "{fragment}"
        );
    }
}
