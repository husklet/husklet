// Corpus data for glsl_es_corpus.rs (included via include!). ANGLE-style GLSL-ES vertex/fragment shaders,
// one construct-family per entry. `Pass` = must reach a valid wgpu module after normalization; `NagaLimit`
// = a genuine naga-24 wall documented for the record.

#[rustfmt::skip]
const CORPUS: &[Case] = &[

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

// -------- uniform blocks (std140), block.member access --------
vs("ubo__std140_mat4", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Matrices { mat4 mvp; vec4 tint; } u;
layout(location = 0) in vec3 aPos;
layout(location = 0) out vec4 vTint;
void main() {
    vTint = u.tint;
    gl_Position = u.mvp * vec4(aPos, 1.0);
}
"#, Pass),

fs("ubo__member_access", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Params { vec4 base; vec4 scale; float gamma; } p;
layout(location = 0) out vec4 o;
void main() { o = pow(p.base * p.scale, vec4(p.gamma)); }
"#, Pass),

fs("ubo__array_member", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Palette { vec4 colors[4]; } pal;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() {
    int idx = int(vUV.x * 4.0);
    o = pal.colors[idx];
}
"#, Pass),

vs("ubo__mat3_mat4", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Xf { mat3 m3; mat4 m4; } x;
layout(location = 0) in vec3 aPos;
void main() {
    vec3 b = x.m3 * aPos;
    gl_Position = x.m4 * vec4(b, 1.0);
}
"#, Pass),

// 2-ROW matrices (mat2 / matNx2) inside a std140 uniform block. naga-24's glsl-in rejects them directly
// (`front/glsl/offset.rs`, `UnsupportedMatrixTypeInStd140`, guarded by `rows == VectorSize::Bi`: it does not
// model the 16-byte column padding std140 gives a 2-row matrix). `glsl_es::split_std140_mat2` rewrites each
// such member to `vec4 M__col[N]` — the IDENTICAL std140 bytes, since std140 already pads every 2-row column
// to a vec4 slot — and reconstructs `matN2(M__col[0].xy, …)` at each use. ANGLE emits mat2 UBOs for 2D
// transforms, so this is the Chrome path. (mat3/mat4, above, are accepted natively.)
vs("ubo__mat2_std140", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Xf { mat2 m2; } x;
layout(location = 0) in vec2 aPos;
void main() { gl_Position = vec4(x.m2 * aPos, 0.0, 1.0); }
"#, Pass),

vs("ubo__mat3x2_std140", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Xf { mat3x2 m; vec4 tint; } x;
layout(location = 0) in vec3 aPos;
layout(location = 0) out vec4 vTint;
void main() {
    vTint = x.tint;
    gl_Position = vec4(x.m * aPos, 0.0, 1.0);
}
"#, Pass),

vs("ubo__mat4x2_std140", r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Xf { mat4x2 m; } x;
layout(location = 0) in vec4 aPos;
void main() {
    vec2 p = x.m * aPos;
    gl_Position = vec4(p, x.m[1][0], 1.0);
}
"#, Pass),

// -------- gl_* builtins --------
vs("builtin__vertexid_instanceid", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 vColor;
void main() {
    int id = gl_VertexID + gl_InstanceID;
    vec2 p = vec2(float(id & 1), float((id >> 1) & 1));
    vColor = vec4(p, 0.0, 1.0);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#, Pass),

// The corpus's ONE inherent naga-24 wall: WGSL has no point-size builtin, so naga's `wgsl-out` cannot
// represent `gl_PointSize` at all (`Unsupported builtin PointSize`). No textual normalization can lower it
// faithfully — the only "fix" would be to silently drop the write, which fakes a green while discarding the
// point size the shader asked for. So it stays documented as a real limit (the real Chrome/GskGpu path
// draws instanced quads, never points, so it never emits this).
vs("builtin__gl_pointsize", r#"#version 300 es
precision highp float;
void main() {
    gl_Position = vec4(0.0, 0.0, 0.0, 1.0);
    gl_PointSize = 4.0;
}
"#, NagaLimit("naga-24 wgsl-out: Unsupported builtin PointSize (WGSL has no point-size builtin; inherent, not textually lowerable)")),

fs("builtin__gl_fragcoord", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() {
    o = vec4(gl_FragCoord.xy * 0.01, 0.0, 1.0);
}
"#, Pass),

fs("builtin__frontfacing_discard", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() {
    if (!gl_FrontFacing) discard;
    o = vec4(1.0);
}
"#, Pass),

// -------- combined samplers: global, function param, cube, array, fetch --------
fs("sampler__global_2d", r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() { o = texture(uTex, vUV); }
"#, Pass),

fs("sampler__textureLod", r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() { o = textureLod(uTex, vUV, 1.0); }
"#, Pass),

fs("sampler__texelFetch_textureSize", r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) out vec4 o;
void main() {
    ivec2 sz = textureSize(uTex, 0);
    o = texelFetch(uTex, ivec2(0, 0), 0) + vec4(float(sz.x));
}
"#, Pass),

fs("sampler__cube", r#"#version 300 es
precision highp float;
uniform samplerCube uCube;
layout(location = 0) in vec3 vDir;
layout(location = 0) out vec4 o;
void main() { o = texture(uCube, vDir); }
"#, Pass),

fs("sampler__2darray", r#"#version 300 es
precision highp float;
uniform sampler2DArray uArr;
layout(location = 0) in vec3 vUVW;
layout(location = 0) out vec4 o;
void main() { o = texture(uArr, vUVW); }
"#, Pass),

fs("sampler__two_globals", r#"#version 300 es
precision highp float;
uniform sampler2D uA;
uniform sampler2D uB;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
void main() { o = texture(uA, vUV) * texture(uB, vUV); }
"#, Pass),

fs("sampler__as_fn_param", r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 o;
vec4 sample_tinted(sampler2D t, vec2 uv, vec4 tint) {
    return texture(t, uv) * tint;
}
void main() { o = sample_tinted(uTex, vUV, vec4(0.5)); }
"#, Pass),

fs("sampler__cube_as_fn_param", r#"#version 300 es
precision highp float;
uniform samplerCube uCube;
layout(location = 0) in vec3 vDir;
layout(location = 0) out vec4 o;
vec4 env(samplerCube c, vec3 d) { return texture(c, d); }
void main() { o = env(uCube, vDir); }
"#, Pass),

// -------- matrices as interface members / locals --------
vs("matrix__mat3x4_attribute", r#"#version 300 es
precision highp float;
layout(location = 0) in mat3x4 aOutline;
layout(location = 3) in vec4 aColor;
layout(location = 0) out vec4 vColor;
void main() {
    vColor = aColor;
    gl_Position = aOutline[0] + aOutline[1] + aOutline[2];
}
"#, Pass),

vs("matrix__mat4_instance_attribute", r#"#version 300 es
precision highp float;
layout(location = 0) in vec3 aPos;
layout(location = 1) in mat4 aModel;
void main() { gl_Position = aModel * vec4(aPos, 1.0); }
"#, Pass),

fs("matrix__local_construct_ops", r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() {
    mat3 m = mat3(1.0);
    mat4x3 r = mat4x3(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5);
    vec3 v = m * (r * vec4(1.0, 2.0, 3.0, 1.0));
    o = vec4(v, 1.0);
}
"#, Pass),

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
