// x87 FPREM/FPREM1 (exact partial remainder) + FSCALE (x*2^trunc(y)) + FABS/FCHS/FSQRT.
// Inputs chosen so a single FPREM completes; results are exact and hashed as double bits.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
#define TGT

#if defined(__x86_64__)
static long double x87_fprem(long double a, long double b){
  long double r; __asm__ volatile("fprem" : "=t"(r) : "0"(a),"u"(b)); return r;
}
static long double x87_fprem1(long double a, long double b){
  long double r; __asm__ volatile("fprem1" : "=t"(r) : "0"(a),"u"(b)); return r;
}
static long double x87_fscale(long double a, long double k){
  long double r; __asm__ volatile("fscale" : "=t"(r) : "0"(a),"u"(k)); return r;
}
static long double x87_fsqrt(long double a){ long double r; __asm__ volatile("fsqrt" : "=t"(r) : "0"(a)); return r; }
#else
static long double x87_fprem(long double a, long double b){ return fmodl(a,b); }
static long double x87_fprem1(long double a, long double b){ return remainderl(a,b); }
static long double x87_fscale(long double a, long double k){ return ldexpl(a,(int)truncl(k)); }
static long double x87_fsqrt(long double a){ return sqrtl(a); }
#endif

static uint64_t hashd(uint64_t h, long double v){ double d=(double)v; uint64_t u; memcpy(&u,&d,8); return h*1000003ULL+u; }

TGT int main(void){
  uint64_t h=0, seed=0xB7E151628AED2A6BULL;
  for(int it=0; it<4000; it++){
    seed = seed*6364136223846793005ULL + 1;
    long double a=(long double)((int)((seed>>4)&0xFFFF)-32768)/8.0L;
    long double b=(long double)((int)((seed>>21)&0x7FF)+1)/8.0L; // nonzero divisor, modest exponent spread
    long double k=(long double)((int)((seed>>9)&0x1F)-16);
    h=hashd(h, x87_fprem(a,b));
    h=hashd(h, x87_fprem1(a,b));
    h=hashd(h, x87_fscale(a,k));
    h=hashd(h, fabsl(a));
    h=hashd(h, -a);
  }
  printf("x87prem=%016llx\n",(unsigned long long)h);
  return 0;
}
