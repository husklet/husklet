// MOVNTDQA (66 0F38 2A, streaming load) + MASKMOVDQU (66 0F F7, masked byte store).
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <immintrin.h>
__attribute__((target("sse4.1"))) static long movntdqa_test(void){
  int32_t src[4] __attribute__((aligned(16))) = {11,22,33,44};
  __m128i v = _mm_stream_load_si128((__m128i*)src);   // MOVNTDQA
  int32_t o[4]; _mm_storeu_si128((__m128i*)o, v);
  long r=0; for(int i=0;i<4;i++) r += o[i]*(i+1);
  return r;
}
__attribute__((target("sse2"))) static long maskmov_test(void){
  uint8_t dst[16]; memset(dst, 0xAA, 16);
  __m128i v = _mm_set_epi8(16,15,14,13,12,11,10,9,8,7,6,5,4,3,2,1);
  // mask: keep bytes where high bit set. Alternate 0x80/0x00.
  __m128i m = _mm_set_epi8(0x80,0,0x80,0,0x80,0,0x80,0,0x80,0,0x80,0,0x80,0,0x80,0);
  _mm_maskmoveu_si128(v, m, (char*)dst);   // MASKMOVDQU: RDI = dst
  long r=0; for(int i=0;i<16;i++) r += dst[i]*(i+1);
  return r;
}
int main(void){
  printf("ntload r=%ld\n", movntdqa_test() + maskmov_test());
  return 0;
}
