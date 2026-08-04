// FMA3 (VEX): VFMADD/VFMSUB/VFNMADD 132/213/231 ps & pd. Single-rounding fused mul-add.
// Portable path uses fmaf/fma (single rounding) to match FMA3 exactly.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("fma,avx")))
#else
#define TGT
#endif

static uint64_t mix8(uint64_t h, const float *f){ uint32_t u[8]; memcpy(u,f,32); for(int i=0;i<8;i++) h=h*1000003ULL+u[i]; return h; }
static uint64_t mix4d(uint64_t h, const double *d){ uint64_t u[4]; memcpy(u,d,32); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; return h; }

TGT int main(void){
  uint64_t h=0, seed=0xABCD1234EF567890ULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    // fractional operands so FMA's single rounding actually differs from mul-then-add
    float a[8],b[8],c[8]; double da[4],db[4],dc[4];
    for(int i=0;i<8;i++){ a[i]=(float)((int)((seed>>(i*5))&0xFFFF)-32768)/7.0f; b[i]=(float)((int)((seed>>(i*3+1))&0xFFFF)-32768)/13.0f; c[i]=(float)((int)((seed>>(i*4+2))&0xFFFF)-32768)/5.0f; }
    for(int i=0;i<4;i++){ da[i]=(double)((int)((seed>>(i*9))&0xFFFF)-32768)/7.0; db[i]=(double)((int)((seed>>(i*6+1))&0xFFFF)-32768)/13.0; dc[i]=(double)((int)((seed>>(i*7+2))&0xFFFF)-32768)/5.0; }
    float o[8]; double od[4];
#if defined(__x86_64__)
    __m256 va=_mm256_loadu_ps(a),vb=_mm256_loadu_ps(b),vc=_mm256_loadu_ps(c);
    _mm256_storeu_ps(o,_mm256_fmadd_ps(va,vb,vc));  h=mix8(h,o);
    _mm256_storeu_ps(o,_mm256_fmsub_ps(va,vb,vc));  h=mix8(h,o);
    _mm256_storeu_ps(o,_mm256_fnmadd_ps(va,vb,vc)); h=mix8(h,o);
    __m256d vda=_mm256_loadu_pd(da),vdb=_mm256_loadu_pd(db),vdc=_mm256_loadu_pd(dc);
    _mm256_storeu_pd(od,_mm256_fmadd_pd(vda,vdb,vdc)); h=mix4d(h,od);
    _mm256_storeu_pd(od,_mm256_fnmsub_pd(vda,vdb,vdc));h=mix4d(h,od);
    __m128 xa=_mm_loadu_ps(a),xb=_mm_loadu_ps(b),xc=_mm_loadu_ps(c);
    float o4[4]; _mm_storeu_ps(o4,_mm_fmadd_ss(xa,xb,xc)); { uint32_t u[4]; memcpy(u,o4,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
#else
    { float r[8]; for(int i=0;i<8;i++) r[i]=fmaf(a[i],b[i],c[i]);  h=mix8(h,r); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=fmaf(a[i],b[i],-c[i]); h=mix8(h,r); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=fmaf(-a[i],b[i],c[i]); h=mix8(h,r); }
    { double r[4]; for(int i=0;i<4;i++) r[i]=fma(da[i],db[i],dc[i]);  h=mix4d(h,r); }
    { double r[4]; for(int i=0;i<4;i++) r[i]=fma(-da[i],db[i],-dc[i]);h=mix4d(h,r); }
    { float r[4]; r[0]=fmaf(a[0],b[0],c[0]); r[1]=a[1]; r[2]=a[2]; r[3]=a[3]; uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; } // fmadd_ss: lane0 fused, rest = xa
#endif
  }
  printf("fma=%016llx\n",(unsigned long long)h);
  return 0;
}
