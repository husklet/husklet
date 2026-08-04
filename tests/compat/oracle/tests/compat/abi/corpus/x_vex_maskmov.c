// VEX masked memory moves block-exited to do_avx: VMASKMOVPS/PD (0F38 2C/2D load, 2E/2F store),
// VPMASKMOVD/Q (0F38 8C load, 8E store). All were UNIMPLEMENTED (exit 70) on the x86 engine. The mask
// is VEX.vvvv; per element the top bit selects load-from-memory / store-to-memory, else 0 / unchanged.
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
  float f[8]; int32_t mps[8];
  for (int i = 0; i < 8; i++) { f[i] = (float)(i * 3 - 4) + 0.5f; mps[i] = (i % 3 == 0) ? -1 : 0; }
  double dpd[4]; int64_t mpd[4];
  for (int i = 0; i < 4; i++) { dpd[i] = (double)(i * 5 - 3) + 0.5; mpd[i] = (i & 1) ? -1 : 0; }
  int32_t vd[8], md[8]; for (int i = 0; i < 8; i++) { vd[i] = i * 7 - 3; md[i] = (i % 2 == 0) ? -1 : 0; }
  int64_t vq[4], mq[4]; for (int i = 0; i < 4; i++) { vq[i] = i * 111 - 5; mq[i] = (i < 2) ? -1 : 0; }
  uint8_t o[32];
#if defined(__x86_64__)
  __m256 F = _mm256_loadu_ps(f); __m256i MPS = _mm256_loadu_si256((void *)mps);
  float lo[8]; memset(lo, 0, sizeof lo);
  _mm256_storeu_ps(lo, _mm256_maskload_ps(f, MPS)); mixb(lo, 32);
  memset(lo, 0, sizeof lo); _mm256_maskstore_ps(lo, MPS, F); mixb(lo, 32);
  __m256d D = _mm256_loadu_pd(dpd); __m256i MPD = _mm256_loadu_si256((void *)mpd);
  double ld[4]; memset(ld, 0, sizeof ld);
  _mm256_storeu_pd(ld, _mm256_maskload_pd(dpd, MPD)); mixb(ld, 32);
  memset(ld, 0, sizeof ld); _mm256_maskstore_pd(ld, MPD, D); mixb(ld, 32);
  __m256i VD = _mm256_loadu_si256((void *)vd), MD = _mm256_loadu_si256((void *)md);
  memset(o, 0, 32); _mm256_storeu_si256((void *)o, _mm256_maskload_epi32(vd, MD)); mixb(o, 32);
  memset(o, 0, 32); _mm256_maskstore_epi32((int *)o, MD, VD); mixb(o, 32);
  __m256i VQ = _mm256_loadu_si256((void *)vq), MQ = _mm256_loadu_si256((void *)mq);
  memset(o, 0, 32); _mm256_storeu_si256((void *)o, _mm256_maskload_epi64((long long *)vq, MQ)); mixb(o, 32);
  memset(o, 0, 32); _mm256_maskstore_epi64((long long *)o, MQ, VQ); mixb(o, 32);
#else
  // load: dst[i] = mask_sign ? src[i] : 0 ; store: dst[i] = mask_sign ? src[i] : 0 (buffer pre-zeroed)
#define MASKLD(dst, src, msk, n, es) do { memset(dst, 0, (n) * (es)); const uint8_t *sp = (const uint8_t *)(src), \
    *mp = (const uint8_t *)(msk); uint8_t *dp = (uint8_t *)(dst); \
    for (int i = 0; i < (n); i++) if (mp[i * (es) + (es) - 1] & 0x80) memcpy(dp + i * (es), sp + i * (es), (es)); } while (0)
  { float lo[8]; MASKLD(lo, f, mps, 8, 4); mixb(lo, 32); }        // maskload_ps
  { float lo[8]; MASKLD(lo, f, mps, 8, 4); mixb(lo, 32); }        // maskstore_ps (same masked bytes)
  { double ld[4]; MASKLD(ld, dpd, mpd, 4, 8); mixb(ld, 32); }     // maskload_pd
  { double ld[4]; MASKLD(ld, dpd, mpd, 4, 8); mixb(ld, 32); }     // maskstore_pd
  { MASKLD(o, vd, md, 8, 4); mixb(o, 32); }                       // maskload_epi32
  { MASKLD(o, vd, md, 8, 4); mixb(o, 32); }                       // maskstore_epi32
  { MASKLD(o, vq, mq, 4, 8); mixb(o, 32); }                       // maskload_epi64
  { MASKLD(o, vq, mq, 4, 8); mixb(o, 32); }                       // maskstore_epi64
#endif
  printf("vexmaskmov=%016llx\n", (unsigned long long)H);
  return 0;
}
