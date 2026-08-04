// Variable ROL/ROR at widths 8/16/32/64 including count masking (x86 ROL/ROR count&width-1).
#include <stdio.h>
#include <stdint.h>
static uint8_t  r8 (uint8_t v,unsigned n){ n&=7;  return n? (uint8_t)((v<<n)|(v>>(8-n))):v; }
static uint16_t r16(uint16_t v,unsigned n){ n&=15; return n? (uint16_t)((v<<n)|(v>>(16-n))):v; }
static uint32_t r32(uint32_t v,unsigned n){ n&=31; return n? (v<<n)|(v>>(32-n)):v; }
static uint64_t r64(uint64_t v,unsigned n){ n&=63; return n? (v<<n)|(v>>(64-n)):v; }
int main(void){
  uint64_t h=0;
  for(unsigned n=0;n<80;n++){
    h=h*1000003ULL+r8 (0xA5,n);
    h=h*1000003ULL+r16(0xBEEF,n);
    h=h*1000003ULL+r32(0xDEADBEEFu,n);
    h=h*1000003ULL+r64(0x0123456789ABCDEFULL,n);
  }
  printf("rot=%016llx\n",(unsigned long long)h);
  return 0;
}
