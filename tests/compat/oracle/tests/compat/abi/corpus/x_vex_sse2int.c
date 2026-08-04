// VEX-encoded SSE2/AVX2 packed-integer ALU that the translator block-exits to the do_avx softmulator:
// saturating add/sub (VPADDS/VPADDUS/VPSUBS/VPSUBUS b/w), unsigned/signed min/max (VPMINUB/VPMAXUB/
// VPMINSW/VPMAXSW), rounded average (VPAVGB/VPAVGW), word multiply (VPMULLW/VPMULHW/VPMULHUW),
// VPMADDWD, VPSADBW and the scalar-count shifts (VPSRLW/D/Q, VPSRAW/D, VPSLLW/D/Q). All were
// UNIMPLEMENTED (block-exit code 70) on the x86 engine. x86_64 emits the real VEX opcodes; aarch64 runs
// the identical scalar model so the golden is byte-identical cross-arch (native-aarch64 == qemu-x86).
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
static int64_t clampi(int64_t v, int64_t lo, int64_t hi) { return v < lo ? lo : v > hi ? hi : v; }
// signed/unsigned saturating add/sub, byte(es=1)/word(es=2)
static void sat_addsub(uint8_t *d, const uint8_t *a, const uint8_t *b, int W, int es, int uns, int sub) {
  for (int i = 0; i < W; i += es) {
    uint64_t x = 0, y = 0;
    memcpy(&x, a + i, es);
    memcpy(&y, b + i, es);
    int64_t o;
    if (uns) {
      int64_t v = sub ? (int64_t)x - (int64_t)y : (int64_t)x + (int64_t)y;
      o = clampi(v, 0, es == 2 ? 65535 : 255);
    } else {
      int sh = 64 - es * 8;
      int64_t sx = ((int64_t)x << sh) >> sh, sy = ((int64_t)y << sh) >> sh;
      int64_t v = sub ? sx - sy : sx + sy;
      o = clampi(v, es == 2 ? -32768 : -128, es == 2 ? 32767 : 127);
    }
    memcpy(d + i, &o, es);
  }
}
#endif

TGT int main(void) {
  uint8_t a[32], b[32];
  for (int i = 0; i < 32; i++) {
    a[i] = (uint8_t)(i * 7 + 3);
    b[i] = (uint8_t)(i * 5 + 130);
  }
  uint8_t o[32];
#if defined(__x86_64__)
  __m256i A = _mm256_loadu_si256((void *)a), B = _mm256_loadu_si256((void *)b);
  __m128i cnt = _mm_set1_epi64x(3);
#define OUT(x) do { _mm256_storeu_si256((void *)o, (x)); mixb(o, 32); } while (0)
  OUT(_mm256_adds_epi8(A, B));
  OUT(_mm256_adds_epi16(A, B));
  OUT(_mm256_adds_epu8(A, B));
  OUT(_mm256_adds_epu16(A, B));
  OUT(_mm256_subs_epi8(A, B));
  OUT(_mm256_subs_epi16(A, B));
  OUT(_mm256_subs_epu8(A, B));
  OUT(_mm256_subs_epu16(A, B));
  OUT(_mm256_min_epu8(A, B));
  OUT(_mm256_max_epu8(A, B));
  OUT(_mm256_min_epi16(A, B));
  OUT(_mm256_max_epi16(A, B));
  OUT(_mm256_avg_epu8(A, B));
  OUT(_mm256_avg_epu16(A, B));
  OUT(_mm256_mullo_epi16(A, B));
  OUT(_mm256_mulhi_epi16(A, B));
  OUT(_mm256_mulhi_epu16(A, B));
  OUT(_mm256_madd_epi16(A, B));
  OUT(_mm256_sad_epu8(A, B));
  OUT(_mm256_srl_epi16(A, cnt));
  OUT(_mm256_srl_epi32(A, cnt));
  OUT(_mm256_srl_epi64(A, cnt));
  OUT(_mm256_sra_epi16(A, cnt));
  OUT(_mm256_sra_epi32(A, cnt));
  OUT(_mm256_sll_epi16(A, cnt));
  OUT(_mm256_sll_epi32(A, cnt));
  OUT(_mm256_sll_epi64(A, cnt));
#else
  const int W = 32;
  // saturating add/sub: es, uns, sub
  sat_addsub(o, a, b, W, 1, 0, 0); mixb(o, 32); // adds_epi8
  sat_addsub(o, a, b, W, 2, 0, 0); mixb(o, 32); // adds_epi16
  sat_addsub(o, a, b, W, 1, 1, 0); mixb(o, 32); // adds_epu8
  sat_addsub(o, a, b, W, 2, 1, 0); mixb(o, 32); // adds_epu16
  sat_addsub(o, a, b, W, 1, 0, 1); mixb(o, 32); // subs_epi8
  sat_addsub(o, a, b, W, 2, 0, 1); mixb(o, 32); // subs_epi16
  sat_addsub(o, a, b, W, 1, 1, 1); mixb(o, 32); // subs_epu8
  sat_addsub(o, a, b, W, 2, 1, 1); mixb(o, 32); // subs_epu16
  for (int i = 0; i < W; i++) o[i] = a[i] < b[i] ? a[i] : b[i]; // min_epu8
  mixb(o, 32);
  for (int i = 0; i < W; i++) o[i] = a[i] > b[i] ? a[i] : b[i]; // max_epu8
  mixb(o, 32);
  for (int i = 0; i < W; i += 2) { int16_t x, y; memcpy(&x, a + i, 2); memcpy(&y, b + i, 2);
    int16_t v = x < y ? x : y; memcpy(o + i, &v, 2); } // min_epi16
  mixb(o, 32);
  for (int i = 0; i < W; i += 2) { int16_t x, y; memcpy(&x, a + i, 2); memcpy(&y, b + i, 2);
    int16_t v = x > y ? x : y; memcpy(o + i, &v, 2); } // max_epi16
  mixb(o, 32);
  for (int i = 0; i < W; i++) { uint16_t v = (uint16_t)(((uint16_t)a[i] + b[i] + 1) >> 1); o[i] = (uint8_t)v; } // avg_epu8
  mixb(o, 32);
  for (int i = 0; i < W; i += 2) { uint16_t x, y; memcpy(&x, a + i, 2); memcpy(&y, b + i, 2);
    uint16_t v = (uint16_t)(((uint32_t)x + y + 1) >> 1); memcpy(o + i, &v, 2); } // avg_epu16
  mixb(o, 32);
  for (int i = 0; i < W; i += 2) { uint16_t x, y; memcpy(&x, a + i, 2); memcpy(&y, b + i, 2);
    uint16_t v = (uint16_t)(x * y); memcpy(o + i, &v, 2); } // mullo_epi16
  mixb(o, 32);
  for (int i = 0; i < W; i += 2) { int16_t x, y; memcpy(&x, a + i, 2); memcpy(&y, b + i, 2);
    uint16_t v = (uint16_t)(((int32_t)x * y) >> 16); memcpy(o + i, &v, 2); } // mulhi_epi16
  mixb(o, 32);
  for (int i = 0; i < W; i += 2) { uint16_t x, y; memcpy(&x, a + i, 2); memcpy(&y, b + i, 2);
    uint16_t v = (uint16_t)(((uint32_t)x * y) >> 16); memcpy(o + i, &v, 2); } // mulhi_epu16
  mixb(o, 32);
  for (int i = 0; i < W; i += 4) { int16_t x0, x1, y0, y1;
    memcpy(&x0, a + i, 2); memcpy(&x1, a + i + 2, 2); memcpy(&y0, b + i, 2); memcpy(&y1, b + i + 2, 2);
    int32_t v = (int32_t)x0 * y0 + (int32_t)x1 * y1; memcpy(o + i, &v, 4); } // madd_epi16
  mixb(o, 32);
  memset(o, 0, 32);
  for (int q = 0; q < W; q += 8) { int s = 0; for (int k = 0; k < 8; k++) { int df = (int)a[q + k] - b[q + k]; s += df < 0 ? -df : df; }
    uint16_t v = (uint16_t)s; memcpy(o + q, &v, 2); } // sad_epu8
  mixb(o, 32);
  // scalar-count shifts, count = 3
  { int cnt = 3;
    for (int i = 0; i < W; i += 2) { uint16_t x; memcpy(&x, a + i, 2); uint16_t v = (uint16_t)(x >> cnt); memcpy(o + i, &v, 2); } mixb(o, 32); // srl16
    for (int i = 0; i < W; i += 4) { uint32_t x; memcpy(&x, a + i, 4); uint32_t v = x >> cnt; memcpy(o + i, &v, 4); } mixb(o, 32); // srl32
    for (int i = 0; i < W; i += 8) { uint64_t x; memcpy(&x, a + i, 8); uint64_t v = x >> cnt; memcpy(o + i, &v, 8); } mixb(o, 32); // srl64
    for (int i = 0; i < W; i += 2) { int16_t x; memcpy(&x, a + i, 2); int16_t v = (int16_t)(x >> cnt); memcpy(o + i, &v, 2); } mixb(o, 32); // sra16
    for (int i = 0; i < W; i += 4) { int32_t x; memcpy(&x, a + i, 4); int32_t v = x >> cnt; memcpy(o + i, &v, 4); } mixb(o, 32); // sra32
    for (int i = 0; i < W; i += 2) { uint16_t x; memcpy(&x, a + i, 2); uint16_t v = (uint16_t)(x << cnt); memcpy(o + i, &v, 2); } mixb(o, 32); // sll16
    for (int i = 0; i < W; i += 4) { uint32_t x; memcpy(&x, a + i, 4); uint32_t v = x << cnt; memcpy(o + i, &v, 4); } mixb(o, 32); // sll32
    for (int i = 0; i < W; i += 8) { uint64_t x; memcpy(&x, a + i, 8); uint64_t v = x << cnt; memcpy(o + i, &v, 8); } mixb(o, 32); // sll64
  }
#endif
  printf("vexsse2int=%016llx\n", (unsigned long long)H);
  return 0;
}
