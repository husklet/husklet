// A mat3 uniform followed by more uniforms — the classic byte-offset bug: MSL float3x3 is 48 bytes
// (3 columns padded to 16), so uColor/uScale must land at offset 48/64, not 36/40. Renders a transformed
// gradient; wrong offsets => wrong coords/color.
uniform mat3 uTransform;
uniform vec3 uColor;
uniform float uScale;
attribute vec2 aPos;
varying vec3 vColor;
void main() {
    vec3 p = uTransform * vec3(aPos, 1.0);
    vColor = uColor * uScale;
    gl_Position = vec4(p.xy, 0.0, 1.0);
}
