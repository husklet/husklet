// VEX conversions + reciprocal approximations block-exited to do_avx: VCVTPD2DQ, VCVTTPD2DQ,
// VCVTDQ2PD, VRCPPS, VRSQRTPS. All were UNIMPLEMENTED (exit 70) on the x86 engine. VRCPPS/VRSQRTPS are
// modeled at full float precision (1/x, 1/sqrt(x)) to match the x86 oracle (qemu) and the native
// aarch64 golden, both of which use full precision rather than the hardware's ~12-bit approximation.
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
  double dd[4] = {1.5, -2.5, 3.25, -4.75};
  int32_t iv[8] = {1, -2, 3, -4, 5, -6, 7, -8};
  float rf[8];
  for (int i = 0; i < 8; i++) rf[i] = (float)(i + 1);
  uint8_t o[32];
#if defined(__x86_64__)
  __m256d D = _mm256_loadu_pd(dd);
  __m128i I4 = _mm_loadu_si128((void *)iv);
  __m256 RF = _mm256_loadu_ps(rf);
  _mm_storeu_si128((void *)o, _mm256_cvtpd_epi32(D)); mixb(o, 16);   // VCVTPD2DQ (round)
  _mm_storeu_si128((void *)o, _mm256_cvttpd_epi32(D)); mixb(o, 16);  // VCVTTPD2DQ (truncate)
  _mm256_storeu_pd((double *)o, _mm256_cvtepi32_pd(I4)); mixb(o, 32);// VCVTDQ2PD
  _mm256_storeu_ps((float *)o, _mm256_rcp_ps(RF)); mixb(o, 32);      // VRCPPS
  _mm256_storeu_ps((float *)o, _mm256_rsqrt_ps(RF)); mixb(o, 32);    // VRSQRTPS
#else
  { int32_t oo[4]; for (int i = 0; i < 4; i++) oo[i] = (int32_t)__builtin_rint(dd[i]); memcpy(o, oo, 16); mixb(o, 16); }
  { int32_t oo[4]; for (int i = 0; i < 4; i++) oo[i] = (int32_t)__builtin_trunc(dd[i]); memcpy(o, oo, 16); mixb(o, 16); }
  { double oo[4]; for (int i = 0; i < 4; i++) oo[i] = (double)iv[i]; memcpy(o, oo, 32); mixb(o, 32); }
  { float oo[8]; for (int i = 0; i < 8; i++) oo[i] = 1.0f / rf[i]; memcpy(o, oo, 32); mixb(o, 32); }
  { float oo[8]; for (int i = 0; i < 8; i++) oo[i] = 1.0f / sqrtf(rf[i]); memcpy(o, oo, 32); mixb(o, 32); }
#endif
  printf("vexcvt=%016llx\n", (unsigned long long)H);
  return 0;
}
