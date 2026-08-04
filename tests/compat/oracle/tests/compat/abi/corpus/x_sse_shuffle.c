// SSE2 vector shuffles/blends: PSHUFD/PSHUFLW/PSHUFHW/UNPCKLPS/UNPCKHPS + mask blend (PAND/PANDN/POR).
#include <stdio.h>
#include <stdint.h>
#if defined(__x86_64__)
#include <emmintrin.h>
#endif
int main(void){
  uint64_t h=0,seed=0xABCDEF0123456789ULL;
  for(int it=0;it<3000;it++){
    seed=seed*6364136223846793005ULL+1;
    uint32_t w[4]={(uint32_t)seed,(uint32_t)(seed>>16),(uint32_t)(seed>>32),(uint32_t)(seed>>48)};
    uint32_t m[4]={(uint32_t)(seed>>7),0,(uint32_t)(seed>>29),0};
    uint32_t o[4];
#if defined(__x86_64__)
    __m128i v=_mm_loadu_si128((const __m128i*)w), mv=_mm_loadu_si128((const __m128i*)m);
    _mm_storeu_si128((__m128i*)o,_mm_shuffle_epi32(v,_MM_SHUFFLE(2,0,3,1)));            for(int i=0;i<4;i++)h=h*1000003ULL+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_shufflelo_epi16(v,_MM_SHUFFLE(0,1,2,3)));          for(int i=0;i<4;i++)h=h*1000003ULL+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_shufflehi_epi16(v,_MM_SHUFFLE(1,0,3,2)));          for(int i=0;i<4;i++)h=h*1000003ULL+o[i];
    _mm_storeu_si128((__m128i*)o,_mm_unpackhi_epi32(v,mv));                             for(int i=0;i<4;i++)h=h*1000003ULL+o[i];
    // blend: (v & mask) | (mv & ~mask), mask = per-lane all-ones where lane index even
    __m128i mask=_mm_set_epi32(0,-1,0,-1);
    _mm_storeu_si128((__m128i*)o,_mm_or_si128(_mm_and_si128(v,mask),_mm_andnot_si128(mask,mv))); for(int i=0;i<4;i++)h=h*1000003ULL+o[i];
#else
    uint32_t s0[4]={w[1],w[3],w[0],w[2]}; for(int i=0;i<4;i++)h=h*1000003ULL+s0[i]; // shuffle_epi32 imm(2,0,3,1): d[j]=w[sel_j], sels=1,3,0,2
    // shufflelo_epi16 imm(0,1,2,3) on low 4 words reversed; high words unchanged
    { uint16_t lw[8]; for(int i=0;i<4;i++){ lw[i*2]=(uint16_t)w[i]; lw[i*2+1]=(uint16_t)(w[i]>>16);} uint16_t r[8]; r[0]=lw[3];r[1]=lw[2];r[2]=lw[1];r[3]=lw[0]; for(int i=4;i<8;i++)r[i]=lw[i]; uint32_t u[4]; for(int i=0;i<4;i++)u[i]=r[i*2]|((uint32_t)r[i*2+1]<<16); for(int i=0;i<4;i++)h=h*1000003ULL+u[i]; }
    // shufflehi_epi16 imm(1,0,3,2) on high 4 words; low unchanged
    { uint16_t lw[8]; for(int i=0;i<4;i++){ lw[i*2]=(uint16_t)w[i]; lw[i*2+1]=(uint16_t)(w[i]>>16);} uint16_t r[8]; for(int i=0;i<4;i++)r[i]=lw[i]; uint16_t hh[4]={lw[4],lw[5],lw[6],lw[7]}; r[4]=hh[2];r[5]=hh[3];r[6]=hh[0];r[7]=hh[1]; uint32_t u[4]; for(int i=0;i<4;i++)u[i]=r[i*2]|((uint32_t)r[i*2+1]<<16); for(int i=0;i<4;i++)h=h*1000003ULL+u[i]; }
    // unpackhi_epi32(v,mv): interleave high halves: d = v[2],mv[2],v[3],mv[3]
    { uint32_t u[4]={w[2],m[2],w[3],m[3]}; for(int i=0;i<4;i++)h=h*1000003ULL+u[i]; }
    // blend even lanes from v, odd from mv
    { uint32_t u[4]={w[0],m[1],w[2],m[3]}; for(int i=0;i<4;i++)h=h*1000003ULL+u[i]; }
#endif
  }
  printf("shuffle=%016llx\n",(unsigned long long)h);
  return 0;
}
