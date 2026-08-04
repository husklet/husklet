// AVX2 shuffle/blend/permute: VPSHUFB (256, per-lane), VPBLENDVB, VPERMQ, VPBLENDD, VPACKSSWB.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("avx2")))
#else
#define TGT
#endif

static uint64_t mix32(uint64_t h, const uint8_t *b){ for(int i=0;i<32;i++) h=h*1000003ULL+b[i]; return h; }

TGT int main(void){
  uint64_t h=0, seed=0x55AA55AA12345678ULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint8_t a[32], b[32], ctrl[32];
    for(int i=0;i<32;i++){ a[i]=(uint8_t)(seed>>(i&7)); b[i]=(uint8_t)(seed>>((i*3)&15)); ctrl[i]=(uint8_t)(seed>>((i*5)&31)); }
    uint8_t out[32];
#if defined(__x86_64__)
    __m256i va=_mm256_loadu_si256((const __m256i*)a), vb=_mm256_loadu_si256((const __m256i*)b), vc=_mm256_loadu_si256((const __m256i*)ctrl);
    _mm256_storeu_si256((__m256i*)out,_mm256_shuffle_epi8(va,vc)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_blendv_epi8(va,vb,vc)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_permute4x64_epi64(va,0x2D)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_blend_epi32(va,vb,0xC5)); h=mix32(h,out);
    _mm256_storeu_si256((__m256i*)out,_mm256_packs_epi16(va,vb)); h=mix32(h,out);
#else
    // vpshufb: per 128-bit lane; index within-lane, high bit -> 0
    { uint8_t r[32]; for(int L=0;L<2;L++){ const uint8_t*al=a+L*16,*cl=ctrl+L*16; uint8_t*rl=r+L*16; for(int j=0;j<16;j++) rl[j]=(cl[j]&0x80)?0:al[cl[j]&0x0F]; } h=mix32(h,r); }
    // blendv_epi8: byte from b if ctrl sign bit set
    { uint8_t r[32]; for(int i=0;i<32;i++) r[i]=(ctrl[i]&0x80)?b[i]:a[i]; h=mix32(h,r); }
    // permute4x64 imm 0x2D: 64-bit lanes r[k]=a[(imm>>(2k))&3]; imm=0x2D=00101101 -> sels 1,3,2,0
    { uint64_t av[4]; memcpy(av,a,32); uint64_t r[4]; int imm=0x2D; for(int k=0;k<4;k++) r[k]=av[(imm>>(2*k))&3]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    // blend_epi32 imm 0xC5: per 32-bit lane, bit set -> b
    { uint32_t av[8],bv[8],r[8]; memcpy(av,a,32); memcpy(bv,b,32); int imm=0xC5; for(int i=0;i<8;i++) r[i]=(imm>>i&1)?bv[i]:av[i]; uint8_t rb[32]; memcpy(rb,r,32); h=mix32(h,rb); }
    // packs_epi16: per 128-bit lane, 8 words a then 8 words b -> 16 signed-saturated bytes
    { int16_t aw[16],bw[16]; memcpy(aw,a,32); memcpy(bw,b,32); uint8_t r[32];
      for(int L=0;L<2;L++){ for(int j=0;j<8;j++){ int v=aw[L*8+j]; if(v>127)v=127; if(v<-128)v=-128; r[L*16+j]=(uint8_t)(int8_t)v; }
        for(int j=0;j<8;j++){ int v=bw[L*8+j]; if(v>127)v=127; if(v<-128)v=-128; r[L*16+8+j]=(uint8_t)(int8_t)v; } } h=mix32(h,r); }
#endif
  }
  printf("avx2shuf=%016llx\n",(unsigned long long)h);
  return 0;
}
