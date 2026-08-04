// VEX-encoded SSE4.1/AVX2 integer forms block-exited to do_avx: VPMULDQ, VPCMPEQQ, VPCMPGTQ,
// VPMINSD/VPMAXSD/VPMINUD/VPMAXUD, VPMINSB/VPMAXSB, VPMINUW/VPMAXUW, VMOVNTDQA, VPHMINPOSUW, VPTEST,
// VTESTPS/VTESTPD. All were UNIMPLEMENTED (exit 70) on the x86 engine. aarch64 mirrors the do_avx model.
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

TGT int main(void) {
  int32_t d1[8], d2[8];
  for (int i = 0; i < 8; i++) { d1[i] = i * 333 - 1000; d2[i] = (i - 4) * 777 + 250; }
  int8_t bb1[32], bb2[32];
  for (int i = 0; i < 32; i++) { bb1[i] = (int8_t)(i * 9 - 100); bb2[i] = (int8_t)(i * -6 + 50); }
  uint16_t uw[16];
  for (int i = 0; i < 16; i++) uw[i] = (uint16_t)(i * 4013 + 7);
  uint16_t phin[8] = {900, 40000, 12, 65535, 7, 300, 12, 88};
  uint8_t o[32];
  int r;
#if defined(__x86_64__)
  __m256i D = _mm256_loadu_si256((void *)d1), D2 = _mm256_loadu_si256((void *)d2);
  __m256i B = _mm256_loadu_si256((void *)bb1), B2 = _mm256_loadu_si256((void *)bb2);
  __m256i U = _mm256_loadu_si256((void *)uw), U2 = _mm256_loadu_si256((void *)(uw));
  __m256 F = _mm256_castsi256_ps(D), F2 = _mm256_castsi256_ps(D2);
  __m256d G = _mm256_castsi256_pd(D), G2 = _mm256_castsi256_pd(D2);
#define OUT(x) do { _mm256_storeu_si256((void *)o, (x)); mixb(o, 32); } while (0)
  OUT(_mm256_mul_epi32(D, D2));
  OUT(_mm256_cmpeq_epi64(D, D2));
  OUT(_mm256_cmpgt_epi64(D, D2));
  OUT(_mm256_min_epi32(D, D2));
  OUT(_mm256_max_epi32(D, D2));
  OUT(_mm256_min_epu32(D, D2));
  OUT(_mm256_max_epu32(D, D2));
  OUT(_mm256_min_epi8(B, B2));
  OUT(_mm256_max_epi8(B, B2));
  OUT(_mm256_min_epu16(U, U2));
  OUT(_mm256_max_epu16(U, U2));
  OUT(_mm256_stream_load_si256((__m256i *)d1)); // VMOVNTDQA
  r = _mm256_testz_si256(D, D2); mixb(&r, 4);
  r = _mm256_testc_si256(D, D2); mixb(&r, 4);
  r = _mm256_testz_ps(F, F2); mixb(&r, 4);   // VTESTPS
  r = _mm256_testc_ps(F, F2); mixb(&r, 4);
  r = _mm256_testz_pd(G, G2); mixb(&r, 4);   // VTESTPD
  r = _mm256_testc_pd(G, G2); mixb(&r, 4);
  __m128i mn = _mm_minpos_epu16(_mm_loadu_si128((void *)phin));
  _mm_storeu_si128((void *)o, mn); mixb(o, 16); // VPHMINPOSUW
#else
  const int W = 32;
  // pmuldq: per-128-lane even-dword signed products
  for (int lane = 0; lane < W; lane += 16) { int32_t x[4], y[4]; int64_t oo[2];
    memcpy(x, (uint8_t *)d1 + lane, 16); memcpy(y, (uint8_t *)d2 + lane, 16);
    oo[0] = (int64_t)x[0] * y[0]; oo[1] = (int64_t)x[2] * y[2]; memcpy(o + lane, oo, 16); }
  mixb(o, 32);
  for (int i = 0; i < W; i += 8) { uint64_t x, y; memcpy(&x, (uint8_t *)d1 + i, 8); memcpy(&y, (uint8_t *)d2 + i, 8);
    uint64_t v = (x == y) ? ~0ull : 0; memcpy(o + i, &v, 8); } mixb(o, 32); // cmpeq_epi64
  for (int i = 0; i < W; i += 8) { int64_t x, y; memcpy(&x, (uint8_t *)d1 + i, 8); memcpy(&y, (uint8_t *)d2 + i, 8);
    uint64_t v = (x > y) ? ~0ull : 0; memcpy(o + i, &v, 8); } mixb(o, 32); // cmpgt_epi64
  for (int i = 0; i < W; i += 4) { int32_t x, y; memcpy(&x, (uint8_t *)d1 + i, 4); memcpy(&y, (uint8_t *)d2 + i, 4);
    int32_t v = x < y ? x : y; memcpy(o + i, &v, 4); } mixb(o, 32); // min_epi32
  for (int i = 0; i < W; i += 4) { int32_t x, y; memcpy(&x, (uint8_t *)d1 + i, 4); memcpy(&y, (uint8_t *)d2 + i, 4);
    int32_t v = x > y ? x : y; memcpy(o + i, &v, 4); } mixb(o, 32); // max_epi32
  for (int i = 0; i < W; i += 4) { uint32_t x, y; memcpy(&x, (uint8_t *)d1 + i, 4); memcpy(&y, (uint8_t *)d2 + i, 4);
    uint32_t v = x < y ? x : y; memcpy(o + i, &v, 4); } mixb(o, 32); // min_epu32
  for (int i = 0; i < W; i += 4) { uint32_t x, y; memcpy(&x, (uint8_t *)d1 + i, 4); memcpy(&y, (uint8_t *)d2 + i, 4);
    uint32_t v = x > y ? x : y; memcpy(o + i, &v, 4); } mixb(o, 32); // max_epu32
  for (int i = 0; i < W; i++) { int8_t x = bb1[i], y = bb2[i]; int8_t v = x < y ? x : y; o[i] = (uint8_t)v; } mixb(o, 32); // min_epi8
  for (int i = 0; i < W; i++) { int8_t x = bb1[i], y = bb2[i]; int8_t v = x > y ? x : y; o[i] = (uint8_t)v; } mixb(o, 32); // max_epi8
  for (int i = 0; i < W; i += 2) { uint16_t x, y; memcpy(&x, (uint8_t *)uw + i, 2); memcpy(&y, (uint8_t *)uw + i, 2);
    uint16_t v = x < y ? x : y; memcpy(o + i, &v, 2); } mixb(o, 32); // min_epu16 (U==U2)
  for (int i = 0; i < W; i += 2) { uint16_t x, y; memcpy(&x, (uint8_t *)uw + i, 2); memcpy(&y, (uint8_t *)uw + i, 2);
    uint16_t v = x > y ? x : y; memcpy(o + i, &v, 2); } mixb(o, 32); // max_epu16
  memcpy(o, d1, 32); mixb(o, 32); // movntdqa: streaming load == the source bytes
  // testz/testc si256: over full 256 bits (two ymm halves = d1 vs d2)
  { uint64_t za = 0, ca = 0; for (int i = 0; i < W; i += 8) { uint64_t x, y;
      memcpy(&x, (uint8_t *)d1 + i, 8); memcpy(&y, (uint8_t *)d2 + i, 8); za |= (x & y); ca |= (y & ~x); }
    r = (za == 0); mixb(&r, 4); r = (ca == 0); mixb(&r, 4); }
  // testps/testpd: sign-bit only, dword / qword
  { uint64_t za = 0, ca = 0; for (int i = 0; i < W; i += 4) { uint32_t x, y;
      memcpy(&x, (uint8_t *)d1 + i, 4); memcpy(&y, (uint8_t *)d2 + i, 4); uint32_t m = 0x80000000u;
      za |= (x & y & m); ca |= (y & ~x & m); }
    r = (za == 0); mixb(&r, 4); r = (ca == 0); mixb(&r, 4); }
  { uint64_t za = 0, ca = 0; for (int i = 0; i < W; i += 8) { uint64_t x, y;
      memcpy(&x, (uint8_t *)d1 + i, 8); memcpy(&y, (uint8_t *)d2 + i, 8); uint64_t m = 1ull << 63;
      za |= (x & y & m); ca |= (y & ~x & m); }
    r = (za == 0); mixb(&r, 4); r = (ca == 0); mixb(&r, 4); }
  // phminposuw: min unsigned word + index into low two words of a 128-bit result
  { uint16_t best = phin[0]; int idx = 0; for (int i = 1; i < 8; i++) if (phin[i] < best) { best = phin[i]; idx = i; }
    uint16_t oo[8] = {best, (uint16_t)idx, 0, 0, 0, 0, 0, 0}; memcpy(o, oo, 16); mixb(o, 16); }
#endif
  printf("vexsse41int=%016llx\n", (unsigned long long)H);
  return 0;
}
