// SAHF: store AH into the arithmetic flags, then read them back (round-trips CF/PF/AF/ZF/SF).
#include <stdio.h>
#include <stdint.h>
static unsigned sahf_roundtrip(unsigned ahbyte){
#if defined(__x86_64__)
  uint64_t fl;
  __asm__ volatile("movb %b1, %%ah\n\tsahf\n\tpushfq\n\tpopq %0\n\t"
                   : "=r"(fl) : "q"((unsigned char)ahbyte) : "cc","ax");
  return (unsigned)(fl & 0xD5u);   // CF(0) PF(2) AF(4) ZF(6) SF(7)
#else
  return ahbyte & 0xD5u;
#endif
}
int main(void){
  uint64_t h=0;
  for(int i=0;i<50000;i++){ unsigned ah=((unsigned)(i*2654435761u)>>16)&0xff; h=h*1000003ULL+sahf_roundtrip(ah); }
  printf("sahf=%016llx\n",(unsigned long long)h);
  return 0;
}
