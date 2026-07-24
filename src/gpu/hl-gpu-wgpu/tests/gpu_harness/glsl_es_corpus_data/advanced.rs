use super::*;

#[rustfmt::skip]
pub(super) const CASES: &[Case] = &[
// -------- more samplers: shadow, proj, grad, gather --------
fs("sampler__shadow_compare", r#"#version 300 es
precision highp float;
uniform highp sampler2DShadow uShadow;
layout(location = 0) in vec3 vShadowCoord;
layout(location = 0) out vec4 o;
void main() {
    float lit = texture(uShadow, vShadowCoord);
    o = vec4(vec3(lit), 1.0);
}
"#, Pass),

fs("sampler__textureProj", r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) in vec3 vProj;
layout(location = 0) out vec4 o;
void main() { o = textureProj(uTex, vProj); }
"#, Pass),

fs("sampler__textureGrad", r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() { o = textureGrad(uTex, vUV, dFdx(vUV), dFdy(vUV)); }
"#, Pass),

fs("sampler__two_via_helpers", r#"#version 300 es
precision highp float;
uniform sampler2D uA;
uniform samplerCube uB;
layout(location = 0) in vec2 vUV;
layout(location = 1) in vec3 vDir;
layout(location = 0) out vec4 o;
vec4 flat_sample(sampler2D t, vec2 uv) { return texture(t, uv); }
vec4 cube_sample(samplerCube c, vec3 d) { return texture(c, d); }
void main() { o = flat_sample(uA, vUV) + cube_sample(uB, vDir); }
"#, Pass),

// -------- integer / interpolation-qualified interface --------
vs("io__integer_attribs", r#"#version 300 es
precision highp float;
layout(location = 0) in ivec4 aIndices;
layout(location = 1) in uvec2 aFlags;
layout(location = 0) flat out int vSum;
void main() {
    vSum = aIndices.x + aIndices.y + int(aFlags.x);
    gl_Position = vec4(float(aIndices.z), float(aFlags.y), 0.0, 1.0);
}
"#, Pass),

fs("io__flat_int_varying", r#"#version 300 es
precision highp float;
layout(location = 0) flat in int vSum;
layout(location = 0) out vec4 o;
void main() { o = vec4(float(vSum) * 0.1, 0.0, 0.0, 1.0); }
"#, Pass),

vs("matrix__mat4x3_attribute", r#"#version 300 es
precision highp float;
layout(location = 0) in mat4x3 aBones;
layout(location = 4) in vec3 aPos;
void main() {
    vec3 p = aBones * vec4(aPos, 1.0);
    gl_Position = vec4(p, 1.0);
}
"#, Pass),

// -------- more control flow / functions --------
fs("flow__do_while", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform C { int n; } c;
layout(location = 0) out vec4 o;
void main() {
    int i = 0; float s = 0.0;
    do { s += 0.05; ++i; } while (i < c.n);
    o = vec4(s, 0.0, 0.0, 1.0);
}
"#, Pass),

fs("flow__nested_calls", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
float sq(float x) { return x * x; }
float len2(vec2 v) { return sq(v.x) + sq(v.y); }
float falloff(vec2 v) { return 1.0 / (1.0 + len2(v)); }
void main() { o = vec4(vec3(falloff(vUV - 0.5)), 1.0); }
"#, Pass),

fs("scalar__const_global", r#"#version 300 es
precision highp float;
const float kPi = 3.14159265;
const vec3 kGray = vec3(0.5);
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() { o = vec4(kGray * sin(vUV.x * kPi), 1.0); }
"#, Pass),

// -------- structs in std140, packing builtins, uvec bitops --------
fs("ubo__struct_member_std140", r#"#version 300 es
precision highp float;
struct Material { vec4 albedo; vec4 emissive; float roughness; };
layout(std140, binding = 0) uniform Block { Material mat; int count; } b;
layout(location = 0) out vec4 o;
void main() { o = b.mat.albedo * b.mat.roughness + b.mat.emissive * float(b.count); }
"#, Pass),

fs("pack__half_unpack", r#"#version 300 es
precision highp float;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() {
    uint packed = packHalf2x16(vUV);
    vec2 unpacked = unpackHalf2x16(packed);
    o = vec4(unpacked, 0.0, 1.0);
}
"#, Pass),

fs("scalar__uvec_bitops", r#"#version 300 es
precision highp float;
precision highp int;
layout(location = 0) flat in uvec4 vBits;
layout(location = 0) out vec4 o;
void main() {
    uvec4 m = (vBits << 2u) ^ (vBits >> 1u);
    uvec4 masked = m & uvec4(0xFFu);
    o = vec4(masked) / 255.0;
}
"#, Pass),

];
