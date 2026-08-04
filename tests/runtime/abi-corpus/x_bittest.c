// BT/BTS/BTR/BTC with register and memory bit-index (x86 bit-test-and-modify codegen).
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint32_t bits[16]={0}; uint64_t h=0; unsigned idx=12345;
  for(int i=0;i<4000;i++){
    idx = idx*1103515245u + 12345u;
    unsigned b = idx % (16*32);
    unsigned word=b>>5, off=b&31;
    unsigned old=(bits[word]>>off)&1u;   // BT
    switch(i&3){
      case 0: bits[word]|= (1u<<off); break;      // BTS
      case 1: bits[word]&= ~(1u<<off); break;     // BTR
      case 2: bits[word]^= (1u<<off); break;      // BTC
      case 3: break;
    }
    h = h*1000003ULL + old + (unsigned long long)b*7;
  }
  uint64_t pop=0; for(int i=0;i<16;i++) pop+=(uint64_t)__builtin_popcount(bits[i]);
  printf("h=%016llx pop=%llu w0=%08x wF=%08x\n",(unsigned long long)h,(unsigned long long)pop,bits[0],bits[15]);
  return 0;
}
