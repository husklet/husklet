// VEX 64-bit store moves + scalar reciprocals + VDPPD block-exited to do_avx: VMOVLPS/VMOVLPD (store,
// 0F 13), VMOVHPS/VMOVHPD (store, 0F 17), VDPPD (0F3A 41), VRCPSS/VRSQRTSS/VSQRTSS (scalar). All were
// UNIMPLEMENTED (exit 70) on the x86 engine. Scalar rcp/rsqrt use full precision to match qemu + aarch64.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("avx2")))
#else
#define TGT
#endif
static uint64_t H = 0;
static void mixb(const void *p, int n) {
  const uint8_t *b = p;
  for (int i = 0; i < n; i++) H = H * 1000003ULL + b[i];
}

TGT int main(void) {
  float f[4] = {1.5f, -2.25f, 3.75f, -4.0f};
  double dd[2] = {2.5, -8.25}, ee[2] = {3.0, 5.0};
  double scal = 9.0;
  uint8_t o[16];
#if defined(__x86_64__)
  __m128 F = _mm_loadu_ps(f);
  __m128d D = _mm_loadu_pd(dd), E = _mm_loadu_pd(ee);
  float m2[2]; double m1;
  _mm_storel_pi((__m64 *)m2, F); mixb(m2, 8);      // VMOVLPS store (low 64)
  _mm_storeh_pi((__m64 *)m2, F); mixb(m2, 8);      // VMOVHPS store (high 64)
  _mm_storel_pd(&m1, D); mixb(&m1, 8);             // VMOVLPD store
  _mm_storeh_pd(&m1, D); mixb(&m1, 8);             // VMOVHPD store
  _mm_storeu_pd((double *)o, _mm_dp_pd(D, E, 0x31)); mixb(o, 16); // VDPPD
  _mm_storeu_pd((double *)o, _mm_dp_pd(D, E, 0x23)); mixb(o, 16);
  __m128 sf = _mm_set_ss((float)scal);
  _mm_store_ss((float *)o, _mm_sqrt_ss(sf)); mixb(o, 4);   // VSQRTSS
  _mm_store_ss((float *)o, _mm_rcp_ss(sf)); mixb(o, 4);    // VRCPSS
  _mm_store_ss((float *)o, _mm_rsqrt_ss(sf)); mixb(o, 4);  // VRSQRTSS
#else
  { float m2[2] = {f[0], f[1]}; mixb(m2, 8); }   // movlps store: low two floats
  { float m2[2] = {f[2], f[3]}; mixb(m2, 8); }   // movhps store: high two floats
  { double m1 = dd[0]; mixb(&m1, 8); }           // movlpd store
  { double m1 = dd[1]; mixb(&m1, 8); }           // movhpd store
  { int imm = 0x31; double sum = 0; for (int i = 0; i < 2; i++) if (imm & (0x10 << i)) sum += dd[i] * ee[i];
    double oo[2]; for (int i = 0; i < 2; i++) oo[i] = (imm & (1 << i)) ? sum : 0.0; memcpy(o, oo, 16); mixb(o, 16); }
  { int imm = 0x23; double sum = 0; for (int i = 0; i < 2; i++) if (imm & (0x10 << i)) sum += dd[i] * ee[i];
    double oo[2]; for (int i = 0; i < 2; i++) oo[i] = (imm & (1 << i)) ? sum : 0.0; memcpy(o, oo, 16); mixb(o, 16); }
  { float y = sqrtf((float)scal); mixb(&y, 4); }
  { float y = 1.0f / (float)scal; mixb(&y, 4); }
  { float y = 1.0f / sqrtf((float)scal); mixb(&y, 4); }
#endif
  printf("vexmovdp=%016llx\n", (unsigned long long)H);
  return 0;
}
