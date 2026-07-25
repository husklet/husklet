use super::*;

#[rustfmt::skip]
pub(super) const CASES: &[Case] = &[
// -------- version / profile / precision --------
vs("version__300es_minimal", r#"#version 300 es
void main() { gl_Position = vec4(0.0, 0.0, 0.0, 1.0); }
"#, Pass),

vs("version__310es_minimal", r#"#version 310 es
void main() { gl_Position = vec4(1.0); }
"#, Pass),

fs("precision__statements_and_inline", r#"#version 300 es
precision highp float;
precision mediump int;
precision lowp sampler2D;
layout(location = 0) out vec4 o;
void main() {
    highp float a = 0.25;
    mediump float b = 0.5;
    lowp vec3 c = vec3(a, b, 1.0);
    o = vec4(c, 1.0);
}
"#, Pass),

fs("precision__int_uint_defaults", r#"#version 300 es
precision highp float;
precision highp int;
layout(location = 0) out vec4 o;
void main() {
    highp int i = 7;
    highp uint u = 3u;
    o = vec4(float(i), float(u), 0.0, 1.0);
}
"#, Pass),

// -------- in/out, layout(location) --------
vs("io__in_out_location", r#"#version 300 es
precision highp float;
layout(location = 0) in vec3 aPos;
layout(location = 1) in vec4 aColor;
layout(location = 0) out vec4 vColor;
void main() {
    vColor = aColor;
    gl_Position = vec4(aPos, 1.0);
}
"#, Pass),

fs("io__varying_in", r#"#version 300 es
precision highp float;
layout(location = 0) in vec4 vColor;
layout(location = 0) out vec4 o;
void main() { o = vColor; }
"#, Pass),

vs("io__many_attribs", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 aPos;
layout(location = 1) in vec2 aUV;
layout(location = 2) in vec4 aColor;
layout(location = 3) in float aScale;
layout(location = 0) out vec2 vUV;
layout(location = 1) out vec4 vColor;
void main() {
    vUV = aUV;
    vColor = aColor;
    gl_Position = vec4(aPos * aScale, 0.0, 1.0);
}
"#, Pass),

// -------- MRT + dual-source blend --------
fs("mrt__two_outputs", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 color0;
layout(location = 1) out vec4 color1;
void main() {
    color0 = vec4(1.0, 0.0, 0.0, 1.0);
    color1 = vec4(0.0, 1.0, 0.0, 1.0);
}
"#, Pass),

fs("mrt__four_outputs", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 g0;
layout(location = 1) out vec4 g1;
layout(location = 2) out vec4 g2;
layout(location = 3) out vec4 g3;
void main() {
    g0 = vec4(0.1); g1 = vec4(0.2); g2 = vec4(0.3); g3 = vec4(0.4);
}
"#, Pass),

fs("blend__dual_source_index", r#"#version 300 es
precision highp float;
layout(location = 0, index = 0) out vec4 src0;
layout(location = 0, index = 1) out vec4 src1;
void main() {
    src0 = vec4(1.0, 0.5, 0.25, 1.0);
    src1 = vec4(0.5, 0.5, 0.5, 1.0);
}
"#, Pass),

];
