// AVX 256-bit VROUNDPS/VROUNDPD (all four rounding modes) and VDPPS (128 + per-128-lane 256)
// with well-behaved (non-NaN) float inputs. aarch64 uses the identical scalar rounding/dot-product
// reference (roundeven/trunc/ceil/floor), so the golden is byte-identical cross-arch.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
// x86 DPPS multiplies then adds as separate rounded ops; keep the scalar reference from fusing
// mul+add into an FMA (which aarch64 -O2 would otherwise do), so the dot products round identically.
#pragma STDC FP_CONTRACT OFF
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("avx2,fma")))
#else
#define TGT
#endif
static uint64_t H = 0;
static void mixb(const void *p, int n) {
  const uint8_t *b = p;
  for (int i = 0; i < n; i++) H = H * 1000003ULL + b[i];
}
TGT int main(void) {
  float fa[8], fb[8];
  double da[4];
  for (int i = 0; i < 8; i++) { fa[i] = (float)(i * 5 - 17) * 0.5f + 0.3f; fb[i] = (float)(i * 3 - 4) * 0.25f - 0.7f; }
  for (int i = 0; i < 4; i++) da[i] = (double)(i * 7 - 11) * 0.5 + 0.4;
  float ob[8]; double od[4];
#if defined(__x86_64__)
  __m256 A = _mm256_loadu_ps(fa), B = _mm256_loadu_ps(fb);
  __m256d DA = _mm256_loadu_pd(da);
  __m128 a = _mm_loadu_ps(fa), b = _mm_loadu_ps(fb);
  _mm256_storeu_ps(ob, _mm256_round_ps(A, _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC)); mixb(ob, 32);
  _mm256_storeu_ps(ob, _mm256_round_ps(A, _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC)); mixb(ob, 32);
  _mm256_storeu_ps(ob, _mm256_round_ps(A, _MM_FROUND_TO_POS_INF | _MM_FROUND_NO_EXC)); mixb(ob, 32);
  _mm256_storeu_ps(ob, _mm256_round_ps(A, _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC)); mixb(ob, 32);
  _mm256_storeu_pd(od, _mm256_round_pd(DA, _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC)); mixb(od, 32);
  _mm256_storeu_pd(od, _mm256_round_pd(DA, _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC)); mixb(od, 32);
  _mm_storeu_ps(ob, _mm_dp_ps(a, b, 0x71)); mixb(ob, 16);
  _mm_storeu_ps(ob, _mm_dp_ps(a, b, 0xFF)); mixb(ob, 16);
  _mm256_storeu_ps(ob, _mm256_dp_ps(A, B, 0xF1)); mixb(ob, 32);
  _mm256_storeu_ps(ob, _mm256_dp_ps(A, B, 0x3B)); mixb(ob, 32);
#else
  for (int i = 0; i < 8; i++) ob[i] = __builtin_roundevenf(fa[i]);
  mixb(ob, 32);
  for (int i = 0; i < 8; i++) ob[i] = __builtin_truncf(fa[i]);
  mixb(ob, 32);
  for (int i = 0; i < 8; i++) ob[i] = __builtin_ceilf(fa[i]);
  mixb(ob, 32);
  for (int i = 0; i < 8; i++) ob[i] = __builtin_floorf(fa[i]);
  mixb(ob, 32);
  for (int i = 0; i < 4; i++) od[i] = __builtin_roundeven(da[i]);
  mixb(od, 32);
  for (int i = 0; i < 4; i++) od[i] = __builtin_trunc(da[i]);
  mixb(od, 32);
  // dp_ps 128 imm=0x71 then 0xFF. volatile product forces a rounding boundary so the reference
  // never fuses mul+add (x86 DPPS multiplies and adds as separate rounded operations).
  for (int m = 0; m < 2; m++) {
    int imm = m ? 0xFF : 0x71;
    float sum = 0;
    for (int i = 0; i < 4; i++) {
      volatile float p = fa[i] * fb[i];
      if (imm & (0x10 << i)) sum += p;
    }
    for (int i = 0; i < 4; i++) ob[i] = (imm & (1 << i)) ? sum : 0.0f;
    mixb(ob, 16);
  }
  // dp_ps 256 imm=0xF1 then 0x3B, per 128-lane
  for (int m = 0; m < 2; m++) {
    int imm = m ? 0x3B : 0xF1;
    for (int lane = 0; lane < 8; lane += 4) {
      float sum = 0;
      for (int i = 0; i < 4; i++) {
        volatile float p = fa[lane + i] * fb[lane + i];
        if (imm & (0x10 << i)) sum += p;
      }
      for (int i = 0; i < 4; i++) ob[lane + i] = (imm & (1 << i)) ? sum : 0.0f;
    }
    mixb(ob, 32);
  }
#endif
  printf("avxfp=%016llx\n", (unsigned long long)H);
  return 0;
}
