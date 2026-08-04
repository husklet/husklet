// SSE4.1 ROUNDPS (packed single) with all 4 rounding modes (cross-checks the x87 rounding theme).
// (ROUNDPD packed-double is isolated in x_roundpd — it mistranslates on the x86 engine.)
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
#include <fenv.h>
#if defined(__x86_64__)
#include <smmintrin.h>
#define TGT __attribute__((target("sse4.1")))
#else
#define TGT
#endif

TGT int main(void){
  uint64_t h=0, seed=0xCAFEBABEDEADBEEFULL;
  for(int it=0; it<3000; it++){
    seed = seed*6364136223846793005ULL + 1;
    // fractional values in a modest range: n/16 - 512
    float f[4];
    for(int i=0;i<4;i++) f[i]=(float)((int)((seed>>(i*9))&0x3FFF)) / 16.0f - 512.0f;
#if defined(__x86_64__)
    __m128 vf=_mm_loadu_ps(f);
    float o[4];
    _mm_storeu_ps(o,_mm_round_ps(vf,_MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC)); { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_ps(o,_mm_round_ps(vf,_MM_FROUND_TO_NEG_INF|_MM_FROUND_NO_EXC));    { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_ps(o,_mm_round_ps(vf,_MM_FROUND_TO_POS_INF|_MM_FROUND_NO_EXC));    { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_ps(o,_mm_round_ps(vf,_MM_FROUND_TO_ZERO|_MM_FROUND_NO_EXC));       { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
#else
    { float r[4]; for(int i=0;i<4;i++) r[i]=rintf(f[i]); uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; } // nearest-even (default fenv)
    { float r[4]; for(int i=0;i<4;i++) r[i]=floorf(f[i]); uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    { float r[4]; for(int i=0;i<4;i++) r[i]=ceilf(f[i]);  uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    { float r[4]; for(int i=0;i<4;i++) r[i]=truncf(f[i]); uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
#endif
  }
  printf("sse41round=%016llx\n",(unsigned long long)h);
  return 0;
}
