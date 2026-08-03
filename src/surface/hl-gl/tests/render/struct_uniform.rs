use super::{compile_translated, link_and_forward};

/// The 18 pure-data uniform-structure cases retained from the isolated GLES2 sweep. Each shape is compiled
/// in both stages through the real shim and host GLSL route. Sampler members are intentionally absent: they
/// require a split data/opaque ABI and are not part of aggregate reconstruction.
#[test]
fn historical_uniform_structures_compile_on_both_stages() {
    let shapes = [
        (
            "basic",
            "struct S { float a; vec3 b; }; uniform S u; float readS(S value) { return value.a + value.b.x; }",
            "vec4 result = vec4(readS(u));",
        ),
        (
            "nested",
            "struct T { vec2 a; }; struct S { float a; T b; }; uniform S u;",
            "vec4 result = vec4(u.a + u.b.a.y);",
        ),
        (
            "array_member",
            "struct S { float a[2]; vec2 b; }; uniform S u;",
            "int i = int(coords.x); vec4 result = vec4(u.a[i] + u.b.x);",
        ),
        (
            "struct_array",
            "struct S { float a; vec2 b; }; uniform S u[2];",
            "vec4 result = vec4(u[0].a + u[1].b.y);",
        ),
        (
            "nested_struct_array",
            "struct T { float a; }; struct S { T a[2]; vec2 b; }; uniform S u;",
            "vec4 result = vec4(u.a[0].a + u.a[1].a + u.b.x);",
        ),
        (
            "loop_struct_array",
            "struct S { float a; vec2 b; }; uniform S u[2];",
            "float sum = 0.0; for (int i = 0; i < 2; ++i) sum += u[i].a + u[i].b.x; vec4 result = vec4(sum);",
        ),
        (
            "loop_nested_struct_array",
            "struct T { float a; }; struct S { T a[2]; }; uniform S u[2];",
            "float sum = 0.0; for (int i = 0; i < 2; ++i) sum += u[i].a[i].a; vec4 result = vec4(sum);",
        ),
        (
            "equal",
            "struct S { float a; vec2 b; }; uniform S u[2];",
            "vec4 result = vec4(u[0] == u[1] ? 1.0 : 0.0);",
        ),
        (
            "not_equal",
            "struct S { float a; vec2 b; }; uniform S u[2];",
            "vec4 result = vec4(u[0] != u[1] ? 1.0 : 0.0);",
        ),
    ];

    for (shape, declarations, body) in shapes {
        for vertex in [true, false] {
            let name = format!("{shape}_{}", if vertex { "vertex" } else { "fragment" });
            let (vs, fs) = stage_sources(vertex, declarations, body);
            let (vs_out, fs_out) = link_and_forward(&vs, &fs);
            compile_translated(&vs_out, &fs_out).unwrap_or_else(|error| panic!("{name}: {error}"));
            let active = if vertex { &vs_out } else { &fs_out };
            assert!(
                active.contains("layout(std140, binding = 0) uniform HlUniforms"),
                "{name}: {active}"
            );
            assert!(active.contains("void main()"), "{name}: {active}");
            let main = active.find("void main()").expect("translated main");
            let first_read = active[main..]
                .find(" = u_")
                .map(|offset| main + offset)
                .expect("aggregate reconstruction before guest body");
            if shape == "basic" {
                let helper_call = active[main..]
                    .find("readS(u)")
                    .map(|offset| main + offset)
                    .expect("aggregate helper call survives lowering");
                assert!(first_read < helper_call, "{name}: {active}");
            }
            if matches!(shape, "struct_array" | "loop_struct_array") {
                assert!(active.contains("u[0]."), "{name}: {active}");
                assert!(active.contains("u[1]."), "{name}: {active}");
            }
        }
    }
}

fn stage_sources(vertex: bool, declarations: &str, body: &str) -> (String, String) {
    if vertex {
        return (
            format!(
                "attribute vec4 a_position; attribute vec4 a_coords; varying vec4 v_color; {declarations} \
                 void main() {{ vec4 coords = a_coords; {body} v_color = result; gl_Position = a_position; }}"
            ),
            "precision mediump float; varying vec4 v_color; void main(){ gl_FragColor = v_color; }".to_owned(),
        );
    }
    (
        "attribute vec4 a_position; attribute vec4 a_coords; varying vec4 v_coords; \
         void main(){ v_coords = a_coords; gl_Position = a_position; }"
            .to_owned(),
        format!(
            "precision mediump float; varying vec4 v_coords; {declarations} \
             void main() {{ vec4 coords = v_coords; {body} gl_FragColor = result; }}"
        ),
    )
}
