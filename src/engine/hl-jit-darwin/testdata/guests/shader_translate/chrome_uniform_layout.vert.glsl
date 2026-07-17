// Uniform-layout fixture: a spread of scalar / vector / square + non-square matrix uniforms whose byte
// offsets must match Metal's real `constant Uniforms&` struct layout (float3=16, float3x3=48, float2x2=16,
// float3x2=24, float2x3=32, ...). run_uniform_layout_proof.sh cross-checks uni_layout() against offsets the
// C compiler computes for the equivalent MSL-faithful struct. Declaration order is the layout order.
uniform float uF;
uniform vec2 uV2;
uniform vec3 uV3;
uniform mat3 uM3;
uniform mat2 uM2;
uniform mat3x2 uM32;
uniform mat2x3 uM23;
uniform mat4 uM4;
uniform vec4 uV4;
uniform int uI;
attribute vec4 aPos;
void main() {
    gl_Position = uM4 * aPos + uV4 + vec4(uF, uV2, uV3.x) + vec4(uV3, float(uI));
}
