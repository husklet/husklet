// VEX-encoded SSSE3 integer forms that the translator block-exits to do_avx: VPHADDW/D, VPHADDSW,
// VPHSUBW/D, VPHSUBSW, VPMADDUBSW, VPSIGNB/W/D, VPMULHRSW, VPABSB/W/D. All were UNIMPLEMENTED (exit 70)
// on the x86 engine. The scalar aarch64 model mirrors the do_avx per-128-lane semantics byte-for-byte.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
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
#if !defined(__x86_64__)
static int16_t sat16(int v) { return (int16_t)(v < -32768 ? -32768 : v > 32767 ? 32767 : v); }
#endif

TGT int main(void) {
  int16_t w[16], w2[16];
  int8_t b1[32], b2[32];
  int32_t d1[8], d2[8];
  for (int i = 0; i < 16; i++) { w[i] = (int16_t)(i * 777 - 3000); w2[i] = (int16_t)(i * -555 + 2000); }
  for (int i = 0; i < 32; i++) { b1[i] = (int8_t)(i * 7 - 60); b2[i] = (int8_t)(i * -5 + 40); }
  for (int i = 0; i < 8; i++) { d1[i] = i * 111111 - 400000; d2[i] = i * -99999 + 300000; }
  int8_t bmin[32], bcontrol[32];
  int16_t wmin[16], wcontrol[16];
  int32_t dmin[8], dcontrol[8];
  memcpy(bmin, b1, sizeof(bmin)); memcpy(bcontrol, b2, sizeof(bcontrol));
  memcpy(wmin, w, sizeof(wmin)); memcpy(wcontrol, w2, sizeof(wcontrol));
  memcpy(dmin, d1, sizeof(dmin)); memcpy(dcontrol, d2, sizeof(dcontrol));
  bmin[0] = INT8_MIN; bcontrol[0] = -1;
  wmin[0] = INT16_MIN; wcontrol[0] = -1;
  dmin[0] = INT32_MIN; dcontrol[0] = -1;
  uint8_t o[32];
#if defined(__x86_64__)
  __m256i W = _mm256_loadu_si256((void *)w), W2 = _mm256_loadu_si256((void *)w2);
  __m256i B = _mm256_loadu_si256((void *)b1), B2 = _mm256_loadu_si256((void *)b2);
  __m256i D = _mm256_loadu_si256((void *)d1), D2 = _mm256_loadu_si256((void *)d2);
  __m256i BM = _mm256_loadu_si256((void *)bmin), BC = _mm256_loadu_si256((void *)bcontrol);
  __m256i WM = _mm256_loadu_si256((void *)wmin), WC = _mm256_loadu_si256((void *)wcontrol);
  __m256i DM = _mm256_loadu_si256((void *)dmin), DC = _mm256_loadu_si256((void *)dcontrol);
#define OUT(x) do { _mm256_storeu_si256((void *)o, (x)); mixb(o, 32); } while (0)
  OUT(_mm256_hadd_epi16(W, W2));
  OUT(_mm256_hadd_epi32(D, D2));
  OUT(_mm256_hadds_epi16(W, W2));
  OUT(_mm256_hsub_epi16(W, W2));
  OUT(_mm256_hsub_epi32(D, D2));
  OUT(_mm256_hsubs_epi16(W, W2));
  OUT(_mm256_sign_epi8(BM, BC));
  OUT(_mm256_sign_epi16(WM, WC));
  OUT(_mm256_sign_epi32(DM, DC));
  OUT(_mm256_abs_epi8(BM));
  OUT(_mm256_abs_epi16(WM));
  OUT(_mm256_abs_epi32(DM));
  OUT(_mm256_maddubs_epi16(B, B2));
  OUT(_mm256_mulhrs_epi16(W, W2));
#else
  const uint8_t *A = (const uint8_t *)w, *S = (const uint8_t *)w2; // reused per op below
  (void)A; (void)S;
  // horizontal add/sub, per 128-bit lane, src1 arrays first then src2
  // hadd/hsub word: 03/07 saturate
#define PH16(av, sv, sub, sat) do { \
    const uint8_t *aa = (const uint8_t *)(av), *bb = (const uint8_t *)(sv); \
    for (int lane = 0; lane < 32; lane += 16) { int16_t x[8], y[8], oo[8]; \
      memcpy(x, aa + lane, 16); memcpy(y, bb + lane, 16); \
      for (int i = 0; i < 4; i++) { int va = (sub) ? x[2*i]-x[2*i+1] : x[2*i]+x[2*i+1]; \
        int vb = (sub) ? y[2*i]-y[2*i+1] : y[2*i]+y[2*i+1]; \
        oo[i] = (sat) ? sat16(va) : (int16_t)va; oo[i+4] = (sat) ? sat16(vb) : (int16_t)vb; } \
      memcpy(o + lane, oo, 16); } mixb(o, 32); } while (0)
#define PH32(av, sv, sub) do { \
    const uint8_t *aa = (const uint8_t *)(av), *bb = (const uint8_t *)(sv); \
    for (int lane = 0; lane < 32; lane += 16) { int32_t x[4], y[4], oo[4]; \
      memcpy(x, aa + lane, 16); memcpy(y, bb + lane, 16); \
      oo[0] = (sub) ? x[0]-x[1] : x[0]+x[1]; oo[1] = (sub) ? x[2]-x[3] : x[2]+x[3]; \
      oo[2] = (sub) ? y[0]-y[1] : y[0]+y[1]; oo[3] = (sub) ? y[2]-y[3] : y[2]+y[3]; \
      memcpy(o + lane, oo, 16); } mixb(o, 32); } while (0)
  PH16(w, w2, 0, 0);  // hadd_epi16
  PH32(d1, d2, 0);    // hadd_epi32
  PH16(w, w2, 0, 1);  // hadds_epi16
  PH16(w, w2, 1, 0);  // hsub_epi16
  PH32(d1, d2, 1);    // hsub_epi32
  PH16(w, w2, 1, 1);  // hsubs_epi16
  // psign b/w/d: es
#define PSIGN(av, sv, es) do { const uint8_t *aa = (const uint8_t *)(av), *bb = (const uint8_t *)(sv); \
    for (int i = 0; i < 32; i += (es)) { uint64_t x = 0, y = 0; memcpy(&x, aa + i, (es)); memcpy(&y, bb + i, (es)); \
      uint64_t mask = (UINT64_C(1) << ((es) * 8)) - 1, sign = UINT64_C(1) << ((es) * 8 - 1); \
      uint64_t oo = (y & sign) ? (UINT64_C(0) - x) & mask : y == 0 ? 0 : x; \
      memcpy(o + i, &oo, (es)); } mixb(o, 32); } while (0)
  PSIGN(bmin, bcontrol, 1); // sign_epi8
  PSIGN(wmin, wcontrol, 2);  // sign_epi16
  PSIGN(dmin, dcontrol, 4); // sign_epi32
  // pabs b/w/d
#define PABS(sv, es) do { const uint8_t *bb = (const uint8_t *)(sv); \
    for (int i = 0; i < 32; i += (es)) { uint64_t x = 0; memcpy(&x, bb + i, (es)); \
      uint64_t mask = (UINT64_C(1) << ((es) * 8)) - 1, sign = UINT64_C(1) << ((es) * 8 - 1); \
      uint64_t oo = (x & sign) ? (UINT64_C(0) - x) & mask : x; memcpy(o + i, &oo, (es)); } mixb(o, 32); } while (0)
  PABS(bmin, 1); // abs_epi8
  PABS(wmin, 2);  // abs_epi16
  PABS(dmin, 4); // abs_epi32
  // maddubs: src1 unsigned bytes, src2 signed bytes -> saturated words
  { const uint8_t *aa = (const uint8_t *)b1, *bb = (const uint8_t *)b2;
    for (int lane = 0; lane < 32; lane += 16) { int16_t oo[8];
      for (int k = 0; k < 8; k++) { int p = (int)(uint8_t)aa[lane + 2*k] * (int)(int8_t)bb[lane + 2*k]
          + (int)(uint8_t)aa[lane + 2*k+1] * (int)(int8_t)bb[lane + 2*k+1]; oo[k] = sat16(p); }
      memcpy(o + lane, oo, 16); } mixb(o, 32); }
  // mulhrs
  { for (int i = 0; i < 32; i += 2) { int16_t x, y; memcpy(&x, (uint8_t *)w + i, 2); memcpy(&y, (uint8_t *)w2 + i, 2);
      int16_t v = (int16_t)((((x * y) >> 14) + 1) >> 1); memcpy(o + i, &v, 2); } mixb(o, 32); }
#endif
  printf("vexssse3=%016llx\n", (unsigned long long)H);
  return 0;
}
