// MMX 64-bit: PADDQ, PMADDWD, PSADBW, PACKSSWB, PUNPCKLBW, PADDUSB + EMMS.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#pragma GCC push_options
#pragma GCC target("mmx,sse,sse2")
#include <mmintrin.h>
#include <xmmintrin.h>
#define TGT
#else
#define TGT
#endif

TGT int main(void){
  uint64_t h=0, seed=0xAABBCCDDEEFF0011ULL;
  for(int it=0; it<3000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint64_t a=seed, b=seed*2654435761ULL+0x1357;
    uint8_t ab[8], bb[8]; memcpy(ab,&a,8); memcpy(bb,&b,8);
#if defined(__x86_64__)
    __m64 ma=*(const __m64*)ab, mb=*(const __m64*)bb;
    uint64_t r;
    r=(uint64_t)_mm_add_si64(ma,mb);                 h=h*1000003ULL+r;   // paddq (single 64-bit)
    { __m64 t=_mm_madd_pi16(ma,mb); r=(uint64_t)t;    h=h*1000003ULL+r; } // pmaddwd
    { __m64 t=_mm_sad_pu8(ma,mb);  r=(uint64_t)t;     h=h*1000003ULL+r; } // psadbw
    { __m64 t=_mm_packs_pi16(ma,mb); r=(uint64_t)t;   h=h*1000003ULL+r; } // packsswb
    { __m64 t=_mm_unpacklo_pi8(ma,mb); r=(uint64_t)t; h=h*1000003ULL+r; } // punpcklbw
    { __m64 t=_mm_adds_pu8(ma,mb); r=(uint64_t)t;     h=h*1000003ULL+r; } // paddusb
    _mm_empty();
#else
    // paddq: single 64-bit add
    { uint64_t r=a+b; h=h*1000003ULL+r; }
    // pmaddwd: 4 signed words -> 2 dwords: d0=a0*b0+a1*b1, d1=a2*b2+a3*b3
    { int16_t aw[4],bw[4]; memcpy(aw,ab,8); memcpy(bw,bb,8); int32_t d0=(int32_t)aw[0]*bw[0]+(int32_t)aw[1]*bw[1], d1=(int32_t)aw[2]*bw[2]+(int32_t)aw[3]*bw[3]; uint64_t r=(uint32_t)d0|((uint64_t)(uint32_t)d1<<32); h=h*1000003ULL+r; }
    // psadbw: sum abs diff of 8 bytes -> word0, rest 0
    { uint32_t s=0; for(int i=0;i<8;i++){ int d=(int)ab[i]-(int)bb[i]; s+=(d<0?-d:d); } uint64_t r=s; h=h*1000003ULL+r; }
    // packsswb: 4 words a, 4 words b -> 8 signed-sat bytes
    { int16_t aw[4],bw[4]; memcpy(aw,ab,8); memcpy(bw,bb,8); uint8_t r8[8]; for(int i=0;i<4;i++){ int v=aw[i]; if(v>127)v=127; if(v<-128)v=-128; r8[i]=(uint8_t)(int8_t)v; } for(int i=0;i<4;i++){ int v=bw[i]; if(v>127)v=127; if(v<-128)v=-128; r8[4+i]=(uint8_t)(int8_t)v; } uint64_t r; memcpy(&r,r8,8); h=h*1000003ULL+r; }
    // punpcklbw: interleave low 4 bytes a,b
    { uint8_t r8[8]; for(int i=0;i<4;i++){ r8[i*2]=ab[i]; r8[i*2+1]=bb[i]; } uint64_t r; memcpy(&r,r8,8); h=h*1000003ULL+r; }
    // paddusb: unsigned saturating byte add
    { uint8_t r8[8]; for(int i=0;i<8;i++){ int v=(int)ab[i]+(int)bb[i]; if(v>255)v=255; r8[i]=(uint8_t)v; } uint64_t r; memcpy(&r,r8,8); h=h*1000003ULL+r; }
#endif
  }
  printf("mmx=%016llx\n",(unsigned long long)h);
  return 0;
}
