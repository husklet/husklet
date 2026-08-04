// BT/BTS/BTR/BTC with a memory operand and bit offset LARGER than operand size (x86 addresses
// the containing memory word: effective byte = base + (offset/8)). Probes the wide-offset path.
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
static int bt_mem(const void*base,long bit){ uint64_t f; __asm__ volatile("bt %2,%1\n\t pushfq\n\t pop %0":"=r"(f):"m"(*(const uint8_t*)base),"r"(bit):"cc"); return (int)(f&1); }
static int bts_mem(void*base,long bit){ uint64_t f; __asm__ volatile("bts %2,%1\n\t pushfq\n\t pop %0":"=r"(f),"+m"(*(uint8_t*)base):"r"(bit):"cc"); return (int)(f&1); }
static int btc_mem(void*base,long bit){ uint64_t f; __asm__ volatile("btc %2,%1\n\t pushfq\n\t pop %0":"=r"(f),"+m"(*(uint8_t*)base):"r"(bit):"cc"); return (int)(f&1); }
#else
static int bt_mem(const void*base,long bit){ const uint8_t*p=(const uint8_t*)base; return (p[bit>>3]>>(bit&7))&1; }
static int bts_mem(void*base,long bit){ uint8_t*p=(uint8_t*)base; int old=(p[bit>>3]>>(bit&7))&1; p[bit>>3]|=(uint8_t)(1u<<(bit&7)); return old; }
static int btc_mem(void*base,long bit){ uint8_t*p=(uint8_t*)base; int old=(p[bit>>3]>>(bit&7))&1; p[bit>>3]^=(uint8_t)(1u<<(bit&7)); return old; }
#endif

TGT int main(void){
  uint64_t h=0, seed=0x13579BDF02468ACEULL;
  static uint8_t bits[128]; // static + oversized: btq addresses the containing 8-byte word
  for(int i=0;i<64;i++) bits[i]=(uint8_t)(i*37+1);
  for(int it=0; it<6000; it++){
    seed = seed*6364136223846793005ULL + 1;
    long off=(long)(seed % 512);   // 0..511 bits across the 64-byte region (well beyond 64)
    h=h*1000003ULL + (uint32_t)bt_mem(bits,off);
    h=h*1000003ULL + (uint32_t)bts_mem(bits,(long)((seed>>9)%512));
    h=h*1000003ULL + (uint32_t)btc_mem(bits,(long)((seed>>19)%512));
  }
  for(int i=0;i<64;i++) h=h*1000003ULL + bits[i];
  printf("btmem=%016llx\n",(unsigned long long)h);
  return 0;
}
