// MOVMSKPS/MOVMSKPD sign-bit extraction from vector lanes (exact bit control via integer reinterpret).
#include <stdio.h>
#include <stdint.h>
#if defined(__x86_64__)
#include <emmintrin.h>
#endif
int main(void){
  uint64_t h=0,seed=0xDEADBEEF12345678ULL;
  for(int it=0;it<5000;it++){
    seed=seed*6364136223846793005ULL+1;
    uint32_t l0=(uint32_t)seed,l1=(uint32_t)(seed>>16),l2=(uint32_t)(seed>>32),l3=(uint32_t)(seed>>13);
    uint64_t d0=seed, d1=seed^0xF0F0F0F0F0F0F0F0ULL;
    unsigned ms,md;
#if defined(__x86_64__)
    __m128 vf=_mm_castsi128_ps(_mm_set_epi32((int)l3,(int)l2,(int)l1,(int)l0));
    __m128d vd=_mm_castsi128_pd(_mm_set_epi64x((long long)d1,(long long)d0));
    ms=(unsigned)_mm_movemask_ps(vf); md=(unsigned)_mm_movemask_pd(vd);
#else
    ms=((l0>>31)&1)|(((l1>>31)&1)<<1)|(((l2>>31)&1)<<2)|(((l3>>31)&1)<<3);
    md=((unsigned)(d0>>63)&1)|(((unsigned)(d1>>63)&1)<<1);
#endif
    h=h*1000003ULL+ms; h=h*1000003ULL+md;
  }
  printf("movmsk=%016llx\n",(unsigned long long)h);
  return 0;
}
