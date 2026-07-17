#version 300 es
precision highp float;
layout(std140) uniform GskUniforms {
    mat4 u_projection;
    vec4 u_color;
};
in vec2 aPosition;
out vec4 vColor;
void main() {
    vColor = u_color;
    gl_Position = u_projection * vec4(aPosition, 0.0, 1.0);
}
