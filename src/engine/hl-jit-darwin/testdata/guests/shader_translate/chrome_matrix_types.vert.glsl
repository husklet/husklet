// Exercises every square + non-square matrix uniform type and matrix*vector product, so gl_type_to_msl /
// type_fixups must lower each GLSL matCxR to the right MSL floatCxR and the emitted MSL must compile.
// GLSL matCxR = C columns, R rows (same as MSL), so matCxR * vecC -> vecR.
uniform mat2 m2;
uniform mat3 m3;
uniform mat4 m4;
uniform mat2x3 m23;
uniform mat3x2 m32;
uniform mat2x4 m24;
uniform mat4x2 m42;
uniform mat3x4 m34;
uniform mat4x3 m43;
attribute vec4 aPos;
void main() {
    vec2 a = m2 * aPos.xy;      // float2x2 * float2 -> float2
    vec3 b = m3 * aPos.xyz;     // float3x3 * float3 -> float3
    vec4 c = m4 * aPos;         // float4x4 * float4 -> float4
    vec3 d = m23 * aPos.xy;     // float2x3 * float2 -> float3
    vec2 e = m32 * aPos.xyz;    // float3x2 * float3 -> float2
    vec4 f = m24 * aPos.xy;     // float2x4 * float2 -> float4
    vec2 g = m42 * aPos;        // float4x2 * float4 -> float2
    vec4 h = m34 * aPos.xyz;    // float3x4 * float3 -> float4
    vec3 i = m43 * aPos;        // float4x3 * float4 -> float3
    gl_Position = c + f + h + vec4(a + e + g, 0.0, 0.0) + vec4(b + d + i, 0.0);
}
