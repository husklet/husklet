use super::{compile_translated, link_and_forward};

#[test]
fn historical_sampler_structures_compile_on_both_stages() {
    let shapes = [
        (
            "sampler",
            "struct S { float a; vec3 b; sampler2D c; }; uniform S s;",
            "vec4 result = vec4(texture2D(s.c, coords.xy * s.b.xy + s.b.z).rgb, s.a);",
        ),
        (
            "sampler_nested",
            "struct T { sampler2D a; vec2 b; }; struct S { float a; T b; int c; }; uniform S s;",
            "vec4 result = vec4(texture2D(s.b.a, coords.xy * s.b.b + s.a).rgb, float(s.c));",
        ),
        (
            "sampler_array",
            "struct S { float a; vec3 b; sampler2D c; }; uniform S s[2];",
            "vec4 result = vec4(texture2D(s[1].c, coords.xy * s[0].b.xy + s[1].b.z).rgb, s[0].a);",
        ),
        (
            "sampler_in_function_arg",
            "struct S { sampler2D source; }; vec4 fun(S value) { return texture2D(value.source, vec2(0.5)); } uniform S s;",
            "vec4 result = fun(s);",
        ),
        (
            "sampler_in_array_function_arg",
            "struct S { sampler2D source; }; vec4 fun(S value[2]) { return texture2D(value[0].source, vec2(0.5)); } uniform S s[2];",
            "vec4 result = fun(s);",
        ),
    ];
    for (shape, declarations, body) in shapes {
        for vertex in [true, false] {
            let name = format!("{shape}_{}", if vertex { "vertex" } else { "fragment" });
            let (vs, fs) = stage_sources(vertex, declarations, body);
            let (vs_out, fs_out) = link_and_forward(&vs, &fs);
            compile_translated(&vs_out, &fs_out).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }
}

#[test]
fn mixed_helper_companions_preserve_nested_data_and_runtime_array_indexing() {
    let shapes = [
        (
            "mixed_helper",
            "struct S { float scale; sampler2D source; }; vec4 fun(S value) { return texture2D(value.source, vec2(0.5)) * value.scale; } uniform S s;",
            "vec4 result=fun(s);",
        ),
        (
            "nested_mixed_helper",
            "struct T { vec2 offset; }; struct S { T data; sampler2D source; }; vec4 fun(S value) { return texture2D(value.source, value.data.offset); } uniform S s;",
            "vec4 result=fun(s);",
        ),
        (
            "runtime_data_array_helper",
            "struct S { float values[2]; sampler2D source; }; vec4 fun(S value) { int i=int(value.values[0]); return texture2D(value.source, vec2(0.5))*value.values[i]; } uniform S s;",
            "vec4 result=fun(s);",
        ),
    ];
    for (name, declarations, body) in shapes {
        let (vs, fs) = stage_sources(true, declarations, body);
        let (vs_out, fs_out) = link_and_forward(&vs, &fs);
        compile_translated(&vs_out, &fs_out).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(vs_out.contains("struct hl_data_S"), "{name}: {vs_out}");
    }
}

fn stage_sources(vertex: bool, declarations: &str, body: &str) -> (String, String) {
    if vertex {
        return (
            format!("attribute vec4 a_position; attribute vec4 a_coords; varying vec4 v_color; {declarations} void main() {{ vec4 coords=a_coords; {body} v_color=result; gl_Position=a_position; }}"),
            "precision mediump float; varying vec4 v_color; void main(){ gl_FragColor=v_color; }".to_owned(),
        );
    }
    (
        "attribute vec4 a_position; attribute vec4 a_coords; varying vec4 v_coords; void main(){ v_coords=a_coords; gl_Position=a_position; }".to_owned(),
        format!("precision mediump float; varying vec4 v_coords; {declarations} void main() {{ vec4 coords=v_coords; {body} gl_FragColor=result; }}"),
    )
}
