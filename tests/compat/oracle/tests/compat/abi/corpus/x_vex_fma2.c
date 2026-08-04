// VEX FMA alternating add/sub + scalar round + unaligned load block-exited to do_avx:
// VFMADDSUB132/213/231 PS/PD, VFMSUBADD132/213/231 PS/PD, VROUNDSS/VROUNDSD, VLDDQU. All were
// UNIMPLEMENTED (exit 70) on the x86 engine. FMA is single-rounded (__builtin_fma) matching x86.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
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
#if !defined(__x86_64__)
static double rnd_d(double x, int mode) {
  switch (mode & 3) {
  case 0: return __builtin_roundeven(x);
  case 1: return __builtin_floor(x);
  case 2: return __builtin_ceil(x);
  default: return __builtin_trunc(x);
  }
}
static float rnd_f(float x, int mode) {
  switch (mode & 3) {
  case 0: return __builtin_roundevenf(x);
  case 1: return __builtin_floorf(x);
  case 2: return __builtin_ceilf(x);
  default: return __builtin_truncf(x);
  }
}
#endif

TGT int main(void) {
  float fa[8], fb[8], fc[8];
  double da[4], db[4], dc[4];
  for (int i = 0; i < 8; i++) { fa[i] = (float)(i - 3) + 0.5f; fb[i] = (float)(i * 2 - 5) + 0.25f; fc[i] = (float)(i - 1) - 0.75f; }
  for (int i = 0; i < 4; i++) { da[i] = (double)(i - 2) + 0.5; db[i] = (double)(i * 3 - 4) + 0.25; dc[i] = (double)(i - 1) - 0.5; }
  int32_t iv[8]; for (int i = 0; i < 8; i++) iv[i] = i * 17 - 33;
  uint8_t o[32]; float of[8]; double od[4];
#if defined(__x86_64__)
  __m256 FA = _mm256_loadu_ps(fa), FB = _mm256_loadu_ps(fb), FC = _mm256_loadu_ps(fc);
  __m256d DA = _mm256_loadu_pd(da), DB = _mm256_loadu_pd(db), DC = _mm256_loadu_pd(dc);
  __m128 sf = _mm_set_ss(2.7f); __m128d sd = _mm_set_sd(-3.4);
  _mm256_storeu_ps(of, _mm256_fmaddsub_ps(FA, FB, FC)); mixb(of, 32);
  _mm256_storeu_pd(od, _mm256_fmaddsub_pd(DA, DB, DC)); mixb(od, 32);
  _mm256_storeu_ps(of, _mm256_fmsubadd_ps(FA, FB, FC)); mixb(of, 32);
  _mm256_storeu_pd(od, _mm256_fmsubadd_pd(DA, DB, DC)); mixb(od, 32);
  _mm_store_ss(of, _mm_round_ss(sf, sf, _MM_FROUND_TO_POS_INF | _MM_FROUND_NO_EXC)); mixb(of, 4);
  _mm_store_sd(od, _mm_round_sd(sd, sd, _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC)); mixb(od, 8);
  _mm256_storeu_si256((void *)o, _mm256_lddqu_si256((void *)iv)); mixb(o, 32);
  _mm_storeu_si128((void *)o, _mm_lddqu_si128((void *)iv)); mixb(o, 16);
#else
  // fmaddsub: even lane subtract, odd lane add; fmsubadd: opposite. 213 form: m1=a, m2=b, add=c.
#define FMAS(av, bv, cv, n, dbl, subadd) do { \
    for (int i = 0; i < (n); i++) { int even = (i & 1) == 0; int sub = (subadd) ? !even : even; \
      if (dbl) { double r = __builtin_fma((av)[i], (bv)[i], sub ? -(cv)[i] : (cv)[i]); od[i] = r; } \
      else { float r = __builtin_fmaf((av)[i], (bv)[i], sub ? -(cv)[i] : (cv)[i]); of[i] = r; } } \
    mixb(dbl ? (void *)od : (void *)of, (n) * ((dbl) ? 8 : 4)); } while (0)
  FMAS(fa, fb, fc, 8, 0, 0); // fmaddsub_ps
  FMAS(da, db, dc, 4, 1, 0); // fmaddsub_pd
  FMAS(fa, fb, fc, 8, 0, 1); // fmsubadd_ps
  FMAS(da, db, dc, 4, 1, 1); // fmsubadd_pd
  { float y = rnd_f(2.7f, 2); mixb(&y, 4); }   // round_ss toward +inf
  { double y = rnd_d(-3.4, 1); mixb(&y, 8); }  // round_sd toward -inf
  memcpy(o, iv, 32); mixb(o, 32);              // lddqu 256
  memcpy(o, iv, 16); mixb(o, 16);              // lddqu 128
#endif
  printf("vexfma2=%016llx\n", (unsigned long long)H);
  return 0;
}
