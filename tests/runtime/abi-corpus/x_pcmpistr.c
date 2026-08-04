// SSE4.2 string compare: PCMPISTRI / PCMPISTRM (implicit-length) across the four aggregation
// modes (equal-any, ranges, equal-each, equal-ordered) with polarity. aarch64 uses a scalar
// reference producing the identical index/mask so the golden is byte-identical cross-arch.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
#define TGT __attribute__((target("sse4.2")))
#else
#define TGT
#endif

static uint64_t mix(uint64_t h, uint64_t v){ return h*1000003ULL + v; }

#if !defined(__x86_64__)
// Scalar reference for byte-oriented PCMPISTRI. imm8: bit2..3 aggregation, bit4..5 polarity,
// bit6 index selection (0=lsb,1=msb). Implicit length = up to first NUL.
static int ilen(const uint8_t *p){ int n=0; while(n<16 && p[n]) n++; return n; }
static int istri(const uint8_t *a, const uint8_t *b, int imm){
  int la=ilen(a), lb=ilen(b);
  int agg=(imm>>2)&3, pol=(imm>>4)&3, msb=(imm>>6)&1;
  int res[16]; memset(res,0,sizeof res);
  for(int j=0;j<16;j++){
    int bvalid = j<lb;
    int r=0;
    switch(agg){
      case 0: // equal any: b[j] present in a
        for(int i=0;i<la;i++) if(a[i]==b[j]){ r=1; break; }
        break;
      case 1: // ranges: pairs in a define [lo,hi]
        for(int i=0;i+1<la;i+=2) if(b[j]>=a[i] && b[j]<=a[i+1]){ r=1; break; }
        break;
      case 2: // equal each: a[j]==b[j]
        r = (j<la && j<lb && a[j]==b[j]);
        if(j>=la || j>=lb) r = (j>=la && j>=lb) ? 1 : 0;
        break;
      case 3: { // equal ordered: substring search, a is needle
        r=1;
        for(int i=0;i<la;i++){ int k=j+i; if(k>=16){ r=(i>= (16-j))?1:0; break;}
          int av=1, bv=k<lb; if(!bv){ r=0; break;} if(a[i]!=b[k]){ r=0; break;} (void)av; }
        break; }
    }
    res[j]=r;
  }
  // polarity
  for(int j=0;j<16;j++){
    if(pol==1) res[j]^=1;                 // negate all
    else if(pol==3){ if(j<lb) res[j]^=1; } // negate valid only (masked)
  }
  int idx = msb ? -1 : 16;
  for(int j=0;j<16;j++) if(res[j]){ if(msb) idx=j; else { idx=j; break; } }
  if(idx<0) idx=16;
  return idx;
}
#endif

TGT int main(void){
  static const char *pairs[][2] = {
    {"aeiou","the quick brown"}, {"az","MixedCASE123"}, {"lo","hello world"},
    {"abc","zzabczz"}, {"123","x1y2z3"}, {"","nonempty"}, {"needle","in a needle stack"},
  };
  static const int imms[] = {0x00,0x04,0x08,0x0c,0x14,0x18,0x1c,0x40,0x44};
  uint64_t h=0;
  for(int p=0;p<7;p++){
    uint8_t A[16]={0}, B[16]={0};
    strncpy((char*)A, pairs[p][0], 16); strncpy((char*)B, pairs[p][1], 16);
    for(int m=0;m<9;m++){
      int idx;
#if defined(__x86_64__)
      __m128i va = _mm_loadu_si128((const __m128i*)A);
      __m128i vb = _mm_loadu_si128((const __m128i*)B);
      switch(imms[m]){
        case 0x00: idx=_mm_cmpistri(va,vb,0x00); break;
        case 0x04: idx=_mm_cmpistri(va,vb,0x04); break;
        case 0x08: idx=_mm_cmpistri(va,vb,0x08); break;
        case 0x0c: idx=_mm_cmpistri(va,vb,0x0c); break;
        case 0x14: idx=_mm_cmpistri(va,vb,0x14); break;
        case 0x18: idx=_mm_cmpistri(va,vb,0x18); break;
        case 0x1c: idx=_mm_cmpistri(va,vb,0x1c); break;
        case 0x40: idx=_mm_cmpistri(va,vb,0x40); break;
        case 0x44: idx=_mm_cmpistri(va,vb,0x44); break;
        default: idx=-1;
      }
#else
      idx = istri(A,B,imms[m]);
#endif
      h = mix(h, (uint64_t)(uint32_t)idx);
    }
  }
  printf("pcmpistr=%016llx\n",(unsigned long long)h);
  return 0;
}
