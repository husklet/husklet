// SSE/SSE2 float lanes: ADDPS/MULPS/DIVPS/MINPS/MAXPS/SQRTPS/CVTTPS on exact-valued inputs.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <emmintrin.h>
#endif
static uint32_t fb(float f){ uint32_t u; memcpy(&u,&f,4); return u; }
int main(void){
  uint64_t h=0;
  for(int it=1;it<=3000;it++){
    float a[4]={(float)((it*3)%257),(float)((it*7)%129),(float)((it%64)*(it%64)),(float)((it*it)%4096)};
    float b[4]={(float)((it%17)+1),(float)((it%5)+2),(float)((it%9)+1),(float)((it%3)+4)};
    float o[4];
#if defined(__x86_64__)
    __m128 va=_mm_loadu_ps(a),vb=_mm_loadu_ps(b);
    _mm_storeu_ps(o,_mm_add_ps(va,vb));  for(int i=0;i<4;i++)h=h*1000003ULL+fb(o[i]);
    _mm_storeu_ps(o,_mm_mul_ps(va,vb));  for(int i=0;i<4;i++)h=h*1000003ULL+fb(o[i]);
    _mm_storeu_ps(o,_mm_div_ps(va,vb));  for(int i=0;i<4;i++)h=h*1000003ULL+fb(o[i]);
    _mm_storeu_ps(o,_mm_min_ps(va,vb));  for(int i=0;i<4;i++)h=h*1000003ULL+fb(o[i]);
    _mm_storeu_ps(o,_mm_max_ps(va,vb));  for(int i=0;i<4;i++)h=h*1000003ULL+fb(o[i]);
    _mm_storeu_ps(o,_mm_sqrt_ps(va));    for(int i=0;i<4;i++)h=h*1000003ULL+fb(o[i]);
    __m128i t=_mm_cvttps_epi32(_mm_div_ps(va,vb)); int32_t ti[4]; _mm_storeu_si128((__m128i*)ti,t); for(int i=0;i<4;i++)h=h*1000003ULL+(uint32_t)ti[i];
#else
    for(int i=0;i<4;i++)h=h*1000003ULL+fb(a[i]+b[i]);
    for(int i=0;i<4;i++)h=h*1000003ULL+fb(a[i]*b[i]);
    for(int i=0;i<4;i++)h=h*1000003ULL+fb(a[i]/b[i]);
    for(int i=0;i<4;i++)h=h*1000003ULL+fb(a[i]<b[i]?a[i]:b[i]);
    for(int i=0;i<4;i++)h=h*1000003ULL+fb(a[i]>b[i]?a[i]:b[i]);
    for(int i=0;i<4;i++)h=h*1000003ULL+fb(__builtin_sqrtf(a[i]));
    for(int i=0;i<4;i++)h=h*1000003ULL+(uint32_t)(int32_t)(a[i]/b[i]);
#endif
  }
  printf("fmath=%016llx\n",(unsigned long long)h);
  return 0;
}
