// VEX FP + lane ops block-exited to do_avx: VHADDPS/PD, VHSUBPS/PD, VADDSUBPS/PD, VSQRTPS/PD,
// VBROADCASTF128 / VBROADCASTI128, VPERMPS, VPERMILPS/VPERMILPD (variable). All were UNIMPLEMENTED
// (exit 70) on the x86 engine. FP inputs are exactly representable and results are correctly-rounded
// (add/sub exact, sqrt IEEE round-to-nearest) so aarch64 == x86 == qemu byte-for-byte.
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
  float f[8], g[8];
  double dd[4], ee[4];
  for (int i = 0; i < 8; i++) { f[i] = (float)(i * 3 - 4); g[i] = (float)(i * -2 + 5); }
  for (int i = 0; i < 4; i++) { dd[i] = (double)(i * 5 - 3); ee[i] = (double)(i * -7 + 2); }
  float sf[8] = {2.0f, 3.0f, 5.0f, 7.0f, 11.0f, 0.25f, 40.0f, 1000000.0f};
  double sd[4] = {2.0, 3.0, 10.0, 0.5};
  int32_t iv[8]; for (int i = 0; i < 8; i++) iv[i] = i * 111 - 300;
  uint8_t o[32]; float of[8]; double od[4];
#if defined(__x86_64__)
  __m256 F = _mm256_loadu_ps(f), G = _mm256_loadu_ps(g), SF = _mm256_loadu_ps(sf);
  __m256d D = _mm256_loadu_pd(dd), E = _mm256_loadu_pd(ee), SD = _mm256_loadu_pd(sd);
  __m256i IV = _mm256_loadu_si256((void *)iv);
  __m256i pidx = _mm256_setr_epi32(5, 0, 7, 2, 1, 6, 3, 4);
  _mm256_storeu_ps(of, _mm256_hadd_ps(F, G)); mixb(of, 32);
  _mm256_storeu_ps(of, _mm256_hsub_ps(F, G)); mixb(of, 32);
  _mm256_storeu_pd(od, _mm256_hadd_pd(D, E)); mixb(od, 32);
  _mm256_storeu_pd(od, _mm256_hsub_pd(D, E)); mixb(od, 32);
  _mm256_storeu_ps(of, _mm256_addsub_ps(F, G)); mixb(of, 32);
  _mm256_storeu_pd(od, _mm256_addsub_pd(D, E)); mixb(od, 32);
  _mm256_storeu_ps(of, _mm256_sqrt_ps(SF)); mixb(of, 32);
  _mm256_storeu_pd(od, _mm256_sqrt_pd(SD)); mixb(od, 32);
  _mm256_storeu_ps(of, _mm256_broadcast_ps((__m128 *)sf)); mixb(of, 32);       // VBROADCASTF128
  _mm256_storeu_si256((void *)o, _mm256_broadcastsi128_si256(_mm_loadu_si128((void *)iv))); mixb(o, 32); // VBROADCASTI128
  _mm256_storeu_ps(of, _mm256_permutevar8x32_ps(F, pidx)); mixb(of, 32);       // VPERMPS
  _mm256_storeu_ps(of, _mm256_permutevar_ps(F, pidx)); mixb(of, 32);           // VPERMILPS var
  _mm256_storeu_pd(od, _mm256_permutevar_pd(D, _mm256_setr_epi64x(2, 0, 0, 2))); mixb(od, 32); // VPERMILPD var
#else
  // hadd/hsub ps: per 128-lane [op(f0,f1), op(f2,f3), op(g0,g1), op(g2,g3)] | [f4..f7 | g4..g7]
#define HPS(sub) do { for (int L = 0; L < 8; L += 4) { \
    of[L+0] = (sub)?f[L+0]-f[L+1]:f[L+0]+f[L+1]; of[L+1] = (sub)?f[L+2]-f[L+3]:f[L+2]+f[L+3]; \
    of[L+2] = (sub)?g[L+0]-g[L+1]:g[L+0]+g[L+1]; of[L+3] = (sub)?g[L+2]-g[L+3]:g[L+2]+g[L+3]; } mixb(of, 32); } while (0)
  HPS(0); // hadd_ps
  HPS(1); // hsub_ps
#define HPD(sub) do { for (int L = 0; L < 4; L += 2) { \
    od[L+0] = (sub)?dd[L+0]-dd[L+1]:dd[L+0]+dd[L+1]; od[L+1] = (sub)?ee[L+0]-ee[L+1]:ee[L+0]+ee[L+1]; } mixb(od, 32); } while (0)
  HPD(0); // hadd_pd
  HPD(1); // hsub_pd
  for (int i = 0; i < 8; i++) of[i] = (i & 1) ? f[i] + g[i] : f[i] - g[i]; mixb(of, 32); // addsub_ps
  for (int i = 0; i < 4; i++) od[i] = (i & 1) ? dd[i] + ee[i] : dd[i] - ee[i]; mixb(od, 32); // addsub_pd
  for (int i = 0; i < 8; i++) of[i] = sqrtf(sf[i]); mixb(of, 32); // sqrt_ps
  for (int i = 0; i < 4; i++) od[i] = sqrt(sd[i]); mixb(od, 32);  // sqrt_pd
  for (int i = 0; i < 8; i++) of[i] = sf[i & 3]; mixb(of, 32);    // broadcastf128: low 4 floats to both lanes
  { int32_t oi[8]; for (int i = 0; i < 8; i++) oi[i] = iv[i & 3]; memcpy(o, oi, 32); mixb(o, 32); } // broadcasti128
  { int idxs[8] = {5, 0, 7, 2, 1, 6, 3, 4}; for (int i = 0; i < 8; i++) of[i] = f[idxs[i]]; mixb(of, 32); } // permps (full 256 dword select)
  { int idxs[8] = {5, 0, 7, 2, 1, 6, 3, 4}; for (int L = 0; L < 8; L += 4) for (int j = 0; j < 4; j++)
      of[L + j] = f[L + (idxs[L + j] & 3)]; mixb(of, 32); } // permilps var: per-lane, ctrl bits [1:0]
  { uint64_t ctl[4] = {2, 0, 0, 2}; for (int L = 0; L < 4; L += 2) for (int k = 0; k < 2; k++)
      od[L + k] = dd[L + ((ctl[L + k] >> 1) & 1)]; mixb(od, 32); } // permilpd var: per-lane, ctrl bit 1
#endif
  printf("vexfp=%016llx\n", (unsigned long long)H);
  return 0;
}
