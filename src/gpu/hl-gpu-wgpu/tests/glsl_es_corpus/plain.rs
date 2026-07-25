use super::*;

#[test]
fn es_vertexid_plus_const_fragment_renders_exact_pixel() {
    let mut guard = match exec() {
        Some(g) => g,
        None => return,
    };
    let exec = &mut *guard;
    // 0.2,0.4,0.6 -> 51,102,153 (round(x*255)).
    let fs = r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() { o = vec4(0.2, 0.4, 0.6, 1.0); }
"#;
    let px = draw_plain(exec, fs);
    for chunk in px.chunks_exact(4) {
        let p = [chunk[0], chunk[1], chunk[2], chunk[3]];
        assert!(
            approx(p, [51, 102, 153, 255], 1),
            "ES gl_VertexID triangle + const fragment must fill {:?}, got {p:?}",
            [51, 102, 153, 255]
        );
    }
}

#[test]
fn es_math_builtins_fragment_renders_exact_pixel() {
    let mut guard = match exec() {
        Some(g) => g,
        None => return,
    };
    let exec = &mut *guard;
    // clamp(mix(0,1,0.5)) = 0.5 -> 128 ; smoothstep(0,1,0.5) = 0.5 -> 128 ; abs(-0.25) = 0.25 -> 64.
    let fs = r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() {
    float a = clamp(mix(0.0, 1.0, 0.5), 0.0, 1.0);
    float c = smoothstep(0.0, 1.0, 0.5);
    float d = abs(-0.25);
    o = vec4(a, c, d, 1.0);
}
"#;
    let px = draw_plain(exec, fs);
    for chunk in px.chunks_exact(4) {
        let p = [chunk[0], chunk[1], chunk[2], chunk[3]];
        assert!(
            approx(p, [128, 128, 64, 255], 2),
            "ES math-builtin fragment must fill ~[128,128,64,255], got {p:?}"
        );
    }
}
