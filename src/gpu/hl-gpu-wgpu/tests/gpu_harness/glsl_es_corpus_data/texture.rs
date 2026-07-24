use super::*;

#[rustfmt::skip]
pub(super) const CASES: &[Case] = &[
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

];
