// ISOLATED CANDIDATE BUG: SSE4.1 ROUNDPD (packed double, 0F3A 09) mistranslates on the x86->arm64
// engine — the double-precision round result diverges from the golden (ROUNDPS single works fine, so
// the defect is specific to the 64-bit-lane rounding path). aarch64 golden and qemu-x86_64 agree.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
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
    double d[2];
    for(int i=0;i<2;i++) d[i]=(double)((int)((seed>>(i*13+3))&0x3FFF)) / 16.0 - 512.0;
#if defined(__x86_64__)
    __m128d vd=_mm_loadu_pd(d); double od[2];
    _mm_storeu_pd(od,_mm_round_pd(vd,_MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC)); { uint64_t u[2]; memcpy(u,od,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_pd(od,_mm_round_pd(vd,_MM_FROUND_TO_NEG_INF|_MM_FROUND_NO_EXC));    { uint64_t u[2]; memcpy(u,od,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_pd(od,_mm_round_pd(vd,_MM_FROUND_TO_POS_INF|_MM_FROUND_NO_EXC));    { uint64_t u[2]; memcpy(u,od,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_pd(od,_mm_round_pd(vd,_MM_FROUND_TO_ZERO|_MM_FROUND_NO_EXC));       { uint64_t u[2]; memcpy(u,od,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
#else
    { double r[2]; for(int i=0;i<2;i++) r[i]=rint(d[i]);  uint64_t u[2]; memcpy(u,r,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
    { double r[2]; for(int i=0;i<2;i++) r[i]=floor(d[i]); uint64_t u[2]; memcpy(u,r,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
    { double r[2]; for(int i=0;i<2;i++) r[i]=ceil(d[i]);  uint64_t u[2]; memcpy(u,r,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
    { double r[2]; for(int i=0;i<2;i++) r[i]=trunc(d[i]); uint64_t u[2]; memcpy(u,r,16); for(int i=0;i<2;i++) h=h*1000003ULL+u[i]; }
#endif
  }
  printf("roundpd=%016llx\n",(unsigned long long)h);
  return 0;
}
