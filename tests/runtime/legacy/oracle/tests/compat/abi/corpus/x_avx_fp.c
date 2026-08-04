// AVX (VEX-encoded, 256-bit YMM): VADDPS/VMULPS/VSUBPS/VDIVPS(exact), VADDPD, VBROADCASTSS, VPERM2F128, VEXTRACTF128.
// Integer-valued floats keep every op exact so aarch64 portable path matches bit-for-bit.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("avx")))
#else
#define TGT
#endif

static uint64_t mix8(uint64_t h, const float *f){ uint32_t u[8]; memcpy(u,f,32); for(int i=0;i<8;i++) h=h*1000003ULL+u[i]; return h; }
static uint64_t mix4d(uint64_t h, const double *d){ uint64_t u[4]; memcpy(u,d,32); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; return h; }

TGT int main(void){
  uint64_t h=0, seed=0x51ED9C0FFEE1234ULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    float a[8], b[8]; double da[4], db[4];
    for(int i=0;i<8;i++){ a[i]=(float)(int)((seed>>(i*5))&0x1FF)-256.0f; b[i]=(float)(int)(((seed>>3)>>(i*4))&0xFF)+1.0f; }
    for(int i=0;i<4;i++){ da[i]=(double)(int)((seed>>(i*9))&0x3FF)-512.0; db[i]=(double)(int)((seed>>(i*6+2))&0x3FF)-512.0; }
    float o[8]; double od[4];
#if defined(__x86_64__)
    __m256 va=_mm256_loadu_ps(a), vb=_mm256_loadu_ps(b);
    _mm256_storeu_ps(o,_mm256_add_ps(va,vb));  h=mix8(h,o);
    _mm256_storeu_ps(o,_mm256_mul_ps(va,vb));  h=mix8(h,o);
    _mm256_storeu_ps(o,_mm256_sub_ps(va,vb));  h=mix8(h,o);
    _mm256_storeu_ps(o,_mm256_div_ps(va,vb));  h=mix8(h,o); // b in [1,256] so exact only if divisible; hashed as bits anyway (det.)
    __m256d vda=_mm256_loadu_pd(da), vdb=_mm256_loadu_pd(db);
    _mm256_storeu_pd(od,_mm256_add_pd(vda,vdb)); h=mix4d(h,od);
    _mm256_storeu_ps(o,_mm256_broadcast_ss(&a[3])); h=mix8(h,o);
    _mm256_storeu_ps(o,_mm256_permute2f128_ps(va,vb,0x21)); h=mix8(h,o);
    __m128 lo=_mm256_extractf128_ps(vb,1); float o4[4]; _mm_storeu_ps(o4,lo); { uint32_t u[4]; memcpy(u,o4,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
#else
    { float r[8]; for(int i=0;i<8;i++) r[i]=a[i]+b[i]; h=mix8(h,r); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=a[i]*b[i]; h=mix8(h,r); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=a[i]-b[i]; h=mix8(h,r); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=a[i]/b[i]; h=mix8(h,r); } // same IEEE single-precision divide
    { double r[4]; for(int i=0;i<4;i++) r[i]=da[i]+db[i]; h=mix4d(h,r); }
    { float r[8]; for(int i=0;i<8;i++) r[i]=a[3]; h=mix8(h,r); } // broadcastss
    // permute2f128 imm 0x21: dst[127:0]=sel(0x1)=vb.lo? sel low nibble=1 -> a[4..7]... imm[1:0]=1 -> src1 hi (a high); imm[5:4]=2 -> src2 lo (b low)
    { float r[8]; for(int i=0;i<4;i++) r[i]=a[4+i]; for(int i=0;i<4;i++) r[4+i]=b[i]; h=mix8(h,r); }
    { float r4[4]; for(int i=0;i<4;i++) r4[i]=b[4+i]; uint32_t u[4]; memcpy(u,r4,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; } // extractf128 hi of b
#endif
  }
  printf("avxfp=%016llx\n",(unsigned long long)h);
  return 0;
}
