use super::{compile_translated, link_and_forward};

/// The eight dEQP-GLES2 failures which motivated aggregate equality lowering: basic/nested structures,
/// `==`/`!=`, in both shader stages. These drive the real shim link/forward path and the executor's exact
/// naga desktop route, so a textual rewrite which emits invalid host GLSL cannot read as repaired.
#[test]
fn historical_struct_equality_shaders_compile_on_both_stages() {
    let cases = [
        ("basic_equal_vertex", false, "==", true),
        ("basic_equal_fragment", false, "==", false),
        ("basic_not_equal_vertex", false, "!=", true),
        ("basic_not_equal_fragment", false, "!=", false),
        ("nested_equal_vertex", true, "==", true),
        ("nested_equal_fragment", true, "==", false),
        ("nested_not_equal_vertex", true, "!=", true),
        ("nested_not_equal_fragment", true, "!=", false),
    ];
    for (name, nested, operator, vertex) in cases {
        let structures = if nested {
            "struct T { mediump vec3 a; int b; };\nstruct S { mediump float a; T b; int c; };"
        } else {
            "struct S { mediump float a; mediump vec3 b; int c; };"
        };
        let values = if nested {
            "S a = S(floor(coords.x), T(vec3(0.0, floor(coords.y), 2.3), ui_one), 1); \
             S b = S(floor(coords.x+0.5), T(vec3(0.0, floor(coords.y), 2.3), ui_one), 1); \
             S c = S(floor(coords.x), T(vec3(0.0, floor(coords.y+0.5), 2.3), ui_one), 1); \
             S d = S(floor(coords.x), T(vec3(0.0, floor(coords.y), 2.3), ui_two), 1);"
        } else {
            "S a = S(floor(coords.x), vec3(0.0, floor(coords.y), 2.3), ui_one); \
             S b = S(floor(coords.x+0.5), vec3(0.0, floor(coords.y), 2.3), ui_one); \
             S c = S(floor(coords.x), vec3(0.0, floor(coords.y+0.5), 2.3), ui_one); \
             S d = S(floor(coords.x), vec3(0.0, floor(coords.y), 2.3), ui_two);"
        };
        let comparisons = format!(
            "vec4 result = vec4(0.0, 0.0, 0.0, 1.0); \
             if (a {operator} b) result.x = 1.0; if (a {operator} c) result.y = 1.0; \
             if (a {operator} d) result.z = 1.0;"
        );
        let (vs, fs) = stage_sources(vertex, structures, values, &comparisons);
        let (vs_out, fs_out) = link_and_forward(&vs, &fs);
        compile_translated(&vs_out, &fs_out).unwrap_or_else(|error| panic!("{name}: {error}"));
        let active = if vertex { &vs_out } else { &fs_out };
        assert!(active.contains("hl_struct_equal_S"), "{name}: {active}");
        for right in ["b", "c", "d"] {
            assert!(
                !active.contains(&format!("a {operator} {right}")),
                "{name}: {active}"
            );
        }
    }
}

fn stage_sources(
    vertex: bool,
    structures: &str,
    values: &str,
    comparisons: &str,
) -> (String, String) {
    if vertex {
        return (
            format!(
                "attribute highp vec4 a_position; attribute mediump vec4 a_coords; varying mediump vec4 v_color; \
                 uniform int ui_one; uniform int ui_two; {structures} void main() {{ vec4 coords = a_coords; \
                 {values} {comparisons} v_color = result; gl_Position = a_position; }}"
            ),
            "precision mediump float; varying mediump vec4 v_color; void main(){ gl_FragColor = v_color; }".to_owned(),
        );
    }
    (
        "attribute highp vec4 a_position; attribute mediump vec4 a_coords; varying mediump vec4 v_coords; \
         void main(){ v_coords = a_coords; gl_Position = a_position; }".to_owned(),
        format!(
            "precision mediump float; varying mediump vec4 v_coords; uniform int ui_one; uniform int ui_two; \
             {structures} void main() {{ vec4 coords = v_coords; {values} {comparisons} gl_FragColor = result; }}"
        ),
    )
}

#[test]
fn scalar_and_vector_equality_controls_stay_native() {
    for (name, vs, fs) in [
        (
            "scalar_equality_control",
            "attribute vec4 a_position; void main(){ int a=1; int b=2; bool same = a == b; gl_Position = same ? a_position : a_position; }",
            "precision mediump float; void main(){ gl_FragColor = vec4(1.0); }",
        ),
        (
            "vector_builtin_equality_control",
            "attribute vec4 a_position; void main(){ gl_Position = a_position; }",
            "precision mediump float; void main(){ vec3 a=vec3(1.0); vec3 b=vec3(2.0); bool same = all(equal(a, b)); gl_FragColor = vec4(same ? 1.0 : 0.0); }",
        ),
    ] {
        let (vs_out, fs_out) = link_and_forward(vs, fs);
        compile_translated(&vs_out, &fs_out).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!vs_out.contains("hl_struct_equal_"), "{name}: {vs_out}");
        assert!(!fs_out.contains("hl_struct_equal_"), "{name}: {fs_out}");
    }
}
