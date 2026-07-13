#version 300 es
precision highp float;
layout(std140) uniform GskUniforms {
    mat4 u_projection;
    vec4 u_color;
};
in vec4 vColor;
out vec4 fragColor;
void main() {
    fragColor = vColor;
}
