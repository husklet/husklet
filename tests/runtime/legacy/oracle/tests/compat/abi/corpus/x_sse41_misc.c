// SSE4.1 misc: PTEST (ZF/CF flags), BLENDVPS, INSERTPS, EXTRACTPS, DPPS (dot product).
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <smmintrin.h>
#define TGT __attribute__((target("sse4.1")))
#else
#define TGT
#endif

TGT int main(void){
  uint64_t h=0, seed=0x9E3779B97F4A7C15ULL;
  for(int it=0; it<2000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint32_t a[4], b[4];
    for(int i=0;i<4;i++){ a[i]=(uint32_t)(seed>>(i*8)); b[i]=(uint32_t)(seed>>(i*6+9)); }
    float fa[4], fb[4];
    for(int i=0;i<4;i++){ fa[i]=(float)(int)((seed>>(i*8))&0xFF)-128.0f; fb[i]=(float)(int)((seed>>(i*7+5))&0xFF)-128.0f; }
#if defined(__x86_64__)
    __m128i va=_mm_loadu_si128((const __m128i*)a), vb=_mm_loadu_si128((const __m128i*)b);
    h=h*1000003ULL + (uint32_t)_mm_testz_si128(va,vb);
    h=h*1000003ULL + (uint32_t)_mm_testc_si128(va,vb);
    __m128 vfa=_mm_loadu_ps(fa), vfb=_mm_loadu_ps(fb);
    // blendvps: mask from sign bit of vb (reinterpret b bits as mask)
    __m128 mask=_mm_castsi128_ps(vb);
    float o[4]; _mm_storeu_ps(o,_mm_blendv_ps(vfa,vfb,mask)); { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    _mm_storeu_ps(o,_mm_insert_ps(vfa,vfb,0x6A)); { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    h=h*1000003ULL + (uint32_t)_mm_extract_ps(vfa,2);
    _mm_storeu_ps(o,_mm_dp_ps(vfa,vfb,0xF1)); { uint32_t u[4]; memcpy(u,o,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
#else
    // ptestz: ZF = ((a & b)==0)
    { uint32_t r=(a[0]&b[0])|(a[1]&b[1])|(a[2]&b[2])|(a[3]&b[3]); h=h*1000003ULL+(uint32_t)(r==0); }
    // ptestc: CF = ((~a & b)==0)
    { uint32_t r=((~a[0])&b[0])|((~a[1])&b[1])|((~a[2])&b[2])|((~a[3])&b[3]); h=h*1000003ULL+(uint32_t)(r==0); }
    // blendvps: per-lane pick fb if sign bit of b set
    { float r[4]; for(int i=0;i<4;i++) r[i]=(b[i]&0x80000000u)?fb[i]:fa[i]; uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    // insert_ps imm 0x6A: src_idx=(0x6A>>6)&3=1, dst_idx=(0x6A>>4)&3=2, zmask=0x6A&0xF=0xA
    { float r[4]; memcpy(r,fa,16); r[2]=fb[1]; for(int i=0;i<4;i++) if((0xA>>i)&1) r[i]=0.0f; uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
    // extract_ps lane 2
    { uint32_t u[4]; memcpy(u,fa,16); h=h*1000003ULL+u[2]; }
    // dp_ps imm 0xF1: multiply all 4 lanes, sum, broadcast to lane0 only
    { float s=fa[0]*fb[0]+fa[1]*fb[1]+fa[2]*fb[2]+fa[3]*fb[3]; float r[4]={s,0.0f,0.0f,0.0f}; uint32_t u[4]; memcpy(u,r,16); for(int i=0;i<4;i++) h=h*1000003ULL+u[i]; }
#endif
  }
  printf("sse41misc=%016llx\n",(unsigned long long)h);
  return 0;
}
