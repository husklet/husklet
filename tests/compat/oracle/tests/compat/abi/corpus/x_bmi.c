// BMI1/BMI2/ABM: LZCNT, TZCNT, ANDN, BLSI, BLSR, BLSMSK, BZHI, RORX, MULX, SARX/SHLX/SHRX.
#include <stdio.h>
#include <stdint.h>
#if defined(__x86_64__)
#pragma GCC push_options
#pragma GCC target("bmi,bmi2,lzcnt,abm")
#include <x86intrin.h>
#define TGT
#else
#define TGT
#endif

TGT int main(void){
  uint64_t h=0, seed=0x9E3779B97F4A7C15ULL;
  for(int it=0; it<6000; it++){
    seed = seed*6364136223846793005ULL + 1;
    uint64_t a=seed|1, b=seed*2654435761ULL+3; // a nonzero for lzcnt/tzcnt determinism
    uint32_t sh=(uint32_t)(seed&63);
#if defined(__x86_64__)
    h=h*1000003ULL + (uint32_t)_lzcnt_u64(a);
    h=h*1000003ULL + (uint32_t)_tzcnt_u64(a);
    h=h*1000003ULL + (uint64_t)_andn_u64(a,b);
    h=h*1000003ULL + (uint64_t)_blsi_u64(a);
    h=h*1000003ULL + (uint64_t)_blsr_u64(a);
    h=h*1000003ULL + (uint64_t)_blsmsk_u64(a);
    h=h*1000003ULL + (uint64_t)_bzhi_u64(b,sh);
    { uint64_t rx; __asm__("rorx $23,%1,%0":"=r"(rx):"r"(a)); h=h*1000003ULL + rx; }
    unsigned long long hi; unsigned long long lo=_mulx_u64(a,b,&hi); h=h*1000003ULL + lo + hi;
    h=h*1000003ULL + (uint64_t)((int64_t)a >> sh); // sarx-equivalent value
#else
    h=h*1000003ULL + (uint32_t)__builtin_clzll(a);
    h=h*1000003ULL + (uint32_t)__builtin_ctzll(a);
    h=h*1000003ULL + (uint64_t)(~a & b);                       // andn
    h=h*1000003ULL + (uint64_t)(a & (~a + 1));                 // blsi: lowest set bit
    h=h*1000003ULL + (uint64_t)(a & (a - 1));                  // blsr: clear lowest set
    h=h*1000003ULL + (uint64_t)(a ^ (a - 1));                  // blsmsk
    h=h*1000003ULL + (uint64_t)(sh>=64 ? b : (b & (((uint64_t)1<<sh)-1))); // bzhi
    h=h*1000003ULL + (uint64_t)((a>>23)|(a<<(64-23)));         // rorx by 23
    { unsigned __int128 p=(unsigned __int128)a*b; h=h*1000003ULL + (uint64_t)p + (uint64_t)(p>>64); } // mulx
    h=h*1000003ULL + (uint64_t)((int64_t)a >> sh);
#endif
  }
  printf("bmi=%016llx\n",(unsigned long long)h);
  return 0;
}
