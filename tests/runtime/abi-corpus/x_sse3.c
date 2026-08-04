// SSE3: HADDPS, HSUBPS, ADDSUBPS, MOVSLDUP, MOVSHDUP. Integer-valued floats keep ops exact.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <pmmintrin.h>
#define TGT __attribute__((target("sse3")))
#else
#define TGT
#endif

static uint64_t mixf(uint64_t h, const float *f){ uint32_t u[4]; memcpy(u,f,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; return h; }

TGT int main(void){
  uint64_t h=0, seed=0xF00DFACEC0FFEEULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    float a[4], b[4];
    for(int i=0;i<4;i++){ a[i]=(float)(int)((seed>>(i*8))&0x3FF)-512.0f; b[i]=(float)(int)((seed>>(i*7+3))&0x3FF)-512.0f; }
    float o[4];
#if defined(__x86_64__)
    __m128 va=_mm_loadu_ps(a), vb=_mm_loadu_ps(b);
    _mm_storeu_ps(o,_mm_hadd_ps(va,vb));     h=mixf(h,o);
    _mm_storeu_ps(o,_mm_hsub_ps(va,vb));     h=mixf(h,o);
    _mm_storeu_ps(o,_mm_addsub_ps(va,vb));   h=mixf(h,o);
    _mm_storeu_ps(o,_mm_moveldup_ps(va));    h=mixf(h,o);
    _mm_storeu_ps(o,_mm_movehdup_ps(vb));    h=mixf(h,o);
#else
    { float r[4]={a[0]+a[1],a[2]+a[3],b[0]+b[1],b[2]+b[3]}; h=mixf(h,r); }
    { float r[4]={a[0]-a[1],a[2]-a[3],b[0]-b[1],b[2]-b[3]}; h=mixf(h,r); }
    { float r[4]={a[0]-b[0],a[1]+b[1],a[2]-b[2],a[3]+b[3]}; h=mixf(h,r); } // addsub: even sub, odd add
    { float r[4]={a[0],a[0],a[2],a[2]}; h=mixf(h,r); }  // moveldup: dup even lanes
    { float r[4]={b[1],b[1],b[3],b[3]}; h=mixf(h,r); }  // movehdup: dup odd lanes
#endif
  }
  printf("sse3=%016llx\n",(unsigned long long)h);
  return 0;
}
