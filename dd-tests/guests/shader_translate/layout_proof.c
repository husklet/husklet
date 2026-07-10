// Independent ground-truth for the uniform byte layout the shim computes in uni_layout().
//
// We model each GLSL uniform type as a C type whose size AND alignment mirror Metal's real MSL struct rules
// (float3 pads to 16; matCxR is C columns of a vecR whose alignment sets the column stride: R=2→8, R=3→16,
// R=4→16). Then we let the C COMPILER lay out `struct Uniforms` in the shader's declaration order and read
// the offsets back with offsetof(). This is a genuinely different mechanism from uni_layout()'s hand-rolled
// `(cur+al-1)&~(al-1)` arithmetic, so agreement proves the shim reproduces Metal's layout.
//
// Field list/order MUST match chrome_uniform_layout.vert.glsl. run_uniform_layout_proof.sh diffs this
// program's `LAYOUT name off sz` / `TOTAL n` output against `gl_tr ... --print-layout`.
#include <stddef.h>
#include <stdio.h>

typedef float msl_float;
typedef int   msl_int;
typedef struct __attribute__((aligned(8)))  { float v[2]; }      msl_float2;    //  8 / 8
typedef struct __attribute__((aligned(16))) { float v[3]; }      msl_float3;    // 16 / 16 (float3 pads to 16)
typedef struct __attribute__((aligned(16))) { float v[4]; }      msl_float4;    // 16 / 16
typedef struct __attribute__((aligned(8)))  { msl_float2 c[2]; } msl_float2x2;  // 16 / 8
typedef struct __attribute__((aligned(8)))  { msl_float2 c[3]; } msl_float3x2;  // 24 / 8
typedef struct __attribute__((aligned(16))) { msl_float3 c[2]; } msl_float2x3;  // 32 / 16
typedef struct __attribute__((aligned(16))) { msl_float3 c[3]; } msl_float3x3;  // 48 / 16
typedef struct __attribute__((aligned(16))) { msl_float4 c[4]; } msl_float4x4;  // 64 / 16

struct Uniforms {
    msl_float    uF;
    msl_float2   uV2;
    msl_float3   uV3;
    msl_float3x3 uM3;
    msl_float2x2 uM2;
    msl_float3x2 uM32;
    msl_float2x3 uM23;
    msl_float4x4 uM4;
    msl_float4   uV4;
    msl_int      uI;
};

#define ROW(f) printf("LAYOUT %s %d %d\n", #f, (int)offsetof(struct Uniforms, f), \
                      (int)sizeof(((struct Uniforms *)0)->f))

int main(void) {
    ROW(uF);
    ROW(uV2);
    ROW(uV3);
    ROW(uM3);
    ROW(uM2);
    ROW(uM32);
    ROW(uM23);
    ROW(uM4);
    ROW(uV4);
    ROW(uI);
    // uni_layout() rounds the total up to 16; sizeof already rounds to the struct's 16-byte alignment here.
    printf("TOTAL %d\n", (int)sizeof(struct Uniforms));
    return 0;
}
