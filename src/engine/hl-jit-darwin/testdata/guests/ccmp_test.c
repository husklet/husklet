// Conditional-compare (CCMP/CCMN) + conditional-select idioms. gpg's keyblock search matches a signature
// to a key with chained equalities like `keyid[0]==d[0] && keyid[1]==d[1]` and `fprlen==n && !memcmp(...)`,
// which the compiler lowers to CMP;CCMP;b.cond. If the engine miscomputes the CCMP condition/flags, the
// chain evaluates wrong -> gpg matches the WRONG key (observed apt BADSIG: first keyblock chosen). This
// checks CCMP-heavy boolean chains against a scalar reference, over adversarial value patterns, at 32 and
// 64 bit widths and both signed/unsigned. -O2 emits CCMP for these; results must match the plain logic.
#include <stdio.h>
#include <stdint.h>

static volatile int sink;

// force the compiler to keep these as real runtime compares (not fold): return via ccmp-lowerable exprs
__attribute__((noinline)) static int and_eq64(uint64_t a,uint64_t b,uint64_t c,uint64_t d){ return a==b && c==d; }
__attribute__((noinline)) static int or_eq64 (uint64_t a,uint64_t b,uint64_t c,uint64_t d){ return a==b || c==d; }
__attribute__((noinline)) static int and_eq32(uint32_t a,uint32_t b,uint32_t c,uint32_t d){ return a==b && c==d; }
__attribute__((noinline)) static int rng_chk (int64_t x,int64_t lo,int64_t hi){ return x>=lo && x<=hi; }
__attribute__((noinline)) static int three  (uint64_t a,uint64_t b,uint64_t c,uint64_t d,uint32_t e,uint32_t f){ return a==b && c==d && e==f; }
__attribute__((noinline)) static uint64_t sel(uint64_t a,uint64_t b,uint64_t c,uint64_t d,uint64_t x,uint64_t y){ return (a==b && c<d) ? x : y; }

int main(void){
    int fails=0;
    uint64_t vals[]={0,1,2,0x7fffffffffffffffull,0x8000000000000000ull,0xffffffffffffffffull,
                     0x871920D1991BC93Cull,0x3B4FE6ACC0B21F32ull,0x100000000ull,0xffffffffull};
    int nv=sizeof vals/sizeof vals[0];
    for(int i=0;i<nv;i++)for(int j=0;j<nv;j++)for(int k=0;k<nv;k++)for(int l=0;l<nv;l++){
        uint64_t a=vals[i],b=vals[j],c=vals[k],d=vals[l];
        int r=and_eq64(a,b,c,d); int w=(a==b)&&(c==d);
        if(r!=w){fails++; if(fails<=6)printf("AND_EQ64 %llx==%llx && %llx==%llx got %d want %d\n",(unsigned long long)a,(unsigned long long)b,(unsigned long long)c,(unsigned long long)d,r,w);}
        int ro=or_eq64(a,b,c,d); int wo=(a==b)||(c==d);
        if(ro!=wo){fails++; if(fails<=6)printf("OR_EQ64 got %d want %d\n",ro,wo);}
        int r32=and_eq32((uint32_t)a,(uint32_t)b,(uint32_t)c,(uint32_t)d); int w32=((uint32_t)a==(uint32_t)b)&&((uint32_t)c==(uint32_t)d);
        if(r32!=w32){fails++; if(fails<=6)printf("AND_EQ32 got %d want %d\n",r32,w32);}
        int r3=three(a,b,c,d,(uint32_t)a,(uint32_t)c); int w3=(a==b)&&(c==d)&&((uint32_t)a==(uint32_t)c);
        if(r3!=w3){fails++; if(fails<=6)printf("THREE got %d want %d\n",r3,w3);}
        uint64_t rs=sel(a,b,c,d,0xAA,0xBB); uint64_t ws=((a==b)&&(c<d))?0xAA:0xBB;
        if(rs!=ws){fails++; if(fails<=6)printf("SEL got %llx want %llx\n",(unsigned long long)rs,(unsigned long long)ws);}
        sink=r^ro^r32;
    }
    // signed range checks (CCMP with signed cond) — like index/len bounds in the parser
    int64_t rv[]={-100,-1,0,1,19,20,21,100,0x7fffffffffffffffLL,-0x8000000000000000LL};
    for(unsigned i=0;i<sizeof rv/sizeof rv[0];i++)for(unsigned j=0;j<sizeof rv/sizeof rv[0];j++)for(unsigned k=0;k<sizeof rv/sizeof rv[0];k++){
        int r=rng_chk(rv[i],rv[j],rv[k]); int w=(rv[i]>=rv[j])&&(rv[i]<=rv[k]);
        if(r!=w){fails++; if(fails<=6)printf("RNG %lld in [%lld,%lld] got %d want %d\n",(long long)rv[i],(long long)rv[j],(long long)rv[k],r,w);}
    }
    printf("ccmp %s (fails=%d)\n", fails==0?"OK":"CORRUPT", fails);
    return fails?1:0;
}
