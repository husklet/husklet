// PEXT/PDEP bit gather/scatter reference (x86 BMI2) exercised portably across masks.
#include <stdio.h>
#include <stdint.h>
static uint64_t pext(uint64_t v,uint64_t m){ uint64_t r=0; int k=0; for(int i=0;i<64;i++) if((m>>i)&1){ r|=((v>>i)&1ULL)<<k; k++; } return r; }
static uint64_t pdep(uint64_t v,uint64_t m){ uint64_t r=0; int k=0; for(int i=0;i<64;i++) if((m>>i)&1){ r|=((v>>k)&1ULL)<<i; k++; } return r; }
int main(void){
  uint64_t h=0,v=0x0123456789ABCDEFULL,m=0xF0F0F0F0F0F0F0F0ULL;
  for(int i=0;i<2000;i++){
    v=v*6364136223846793005ULL+1; m=m*2862933555777941757ULL+3037000493ULL;
    uint64_t e=pext(v,m), d=pdep(v,m);
    h=h*1000003ULL+e; h=h*1000003ULL+d;
  }
  printf("bmi2=%016llx\n",(unsigned long long)h);
  return 0;
}
