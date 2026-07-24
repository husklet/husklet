use super::*;

#[rustfmt::skip]
pub(super) const CASES: &[Case] = &[
// -------- arrays / structs --------
fs("array__const_global", r#"#version 300 es
precision highp float;
const vec2 kOffsets[4] = vec2[4](vec2(0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(1.0));
layout(location = 0) out vec4 o;
void main() {
    vec2 acc = vec2(0.0);
    for (int i = 0; i < 4; ++i) acc += kOffsets[i];
    o = vec4(acc, 0.0, 1.0);
}
"#, Pass),

fs("struct__nested", r#"#version 300 es
precision highp float;
struct Light { vec3 pos; vec3 color; };
struct Scene { Light key; Light fill; float ambient; };
layout(location = 0) out vec4 o;
void main() {
    Scene s;
    s.key = Light(vec3(1.0), vec3(1.0, 0.9, 0.8));
    s.fill = Light(vec3(-1.0), vec3(0.2));
    s.ambient = 0.1;
    o = vec4(s.key.color * 0.5 + s.fill.color + s.ambient, 1.0);
}
"#, Pass),

fs("struct__array_of_struct", r#"#version 300 es
precision highp float;
struct Wave { float amp; float freq; };
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() {
    Wave waves[3];
    waves[0] = Wave(1.0, 2.0);
    waves[1] = Wave(0.5, 4.0);
    waves[2] = Wave(0.25, 8.0);
    float h = 0.0;
    for (int i = 0; i < 3; ++i) h += waves[i].amp * sin(waves[i].freq * vUV.x);
    o = vec4(vec3(h), 1.0);
}
"#, Pass),

vs("array__vec_varying", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 aPos;
layout(location = 0) out vec4 vData[3];
void main() {
    vData[0] = vec4(aPos, 0.0, 1.0);
    vData[1] = vec4(1.0, 0.0, 0.0, 1.0);
    vData[2] = vec4(0.0, 1.0, 0.0, 1.0);
    gl_Position = vec4(aPos, 0.0, 1.0);
}
"#, Pass),

// -------- control flow --------
fs("flow__for_const", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() {
    float s = 0.0;
    for (int i = 0; i < 8; ++i) s += float(i) * 0.1;
    o = vec4(s, 0.0, 0.0, 1.0);
}
"#, Pass),

fs("flow__while_dynamic", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform C { int count; } c;
layout(location = 0) out vec4 o;
void main() {
    int i = 0; float s = 0.0;
    while (i < c.count) { s += 0.01; ++i; }
    o = vec4(s, 0.0, 0.0, 1.0);
}
"#, Pass),

fs("flow__if_else_ternary", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() {
    float t;
    if (vUV.x < 0.25) t = 0.0;
    else if (vUV.x < 0.5) t = 0.33;
    else t = 1.0;
    o = vUV.y > 0.5 ? vec4(t) : vec4(1.0 - t);
}
"#, Pass),

fs("flow__switch_return", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform M { int mode; } m;
layout(location = 0) out vec4 o;
vec4 pick(int mode) {
    switch (mode) {
        case 0: return vec4(1.0, 0.0, 0.0, 1.0);
        case 1:
        case 2: return vec4(0.0, 1.0, 0.0, 1.0);
        default: return vec4(0.0, 0.0, 1.0, 1.0);
    }
}
void main() { o = pick(m.mode); }
"#, Pass),

fs("flow__early_return_discard", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() {
    if (vUV.x > vUV.y) { o = vec4(1.0); return; }
    if (vUV.x < 0.1) discard;
    o = vec4(0.0, 0.0, 0.0, 1.0);
}
"#, Pass),

fs("flow__user_fns_inout", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void addOne(inout float x) { x += 1.0; }
float scaled(in float x, out float doubled) { doubled = x * 2.0; return x * 0.5; }
void main() {
    float a = 1.0;
    addOne(a);
    float d;
    float h = scaled(a, d);
    o = vec4(a, d, h, 1.0);
}
"#, Pass),

// -------- scalar types, bitwise, swizzle, math builtins --------
fs("scalar__int_uint_bool_bitwise", r#"#version 300 es
precision highp float;
precision highp int;
layout(location = 0) out vec4 o;
void main() {
    uint a = 0xF0u;
    uint b = 0x0Fu;
    uint c = (a | b) & 0xFFu;
    int s = int(c) << 1;
    int r = s >> 2;
    bool flag = (c ^ 0xFFu) == 0u;
    o = vec4(float(r), float(c), flag ? 1.0 : 0.0, 1.0);
}
"#, Pass),

fs("swizzle__read_write", r#"#version 300 es
precision highp float;
layout(location = 0) in vec4 vColor;
layout(location = 0) out vec4 o;
void main() {
    vec4 c = vColor.bgra;
    c.xy = c.yx;
    c.rgb *= 0.5;
    o = c.wzyx;
}
"#, Pass),

fs("math__builtins_suite", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() {
    vec3 a = vec3(vUV, 0.5);
    vec3 b = vec3(0.25, 0.75, 1.0);
    vec3 r = mix(a, b, 0.5);
    r = clamp(r, 0.0, 1.0);
    r = vec3(mod(r.x, 0.3), fract(r.y), abs(r.z - 1.0));
    float d = dot(a, b);
    vec3 cr = cross(a, b);
    vec3 n = normalize(cr + vec3(0.001));
    float len = length(a);
    r += vec3(step(0.5, r.x), smoothstep(0.0, 1.0, r.y), sign(r.z - 0.5));
    r = pow(r, vec3(2.2));
    r = floor(r) + ceil(fract(r));
    o = vec4(r + d + n + len, 1.0);
}
"#, Pass),

];
