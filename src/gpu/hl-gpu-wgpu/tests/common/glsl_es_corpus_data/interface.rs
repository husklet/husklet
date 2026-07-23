use super::*;

#[rustfmt::skip]
pub(super) const CASES: &[Case] = &[
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

];
