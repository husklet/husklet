// Shift/rotate corner flag semantics. Only x86-DEFINED flag bits are hashed (undefined bits — AF for
// shifts, OF for count!=1, all shift flags for count 0 which are UNCHANGED — are masked identically on
// both sides). Probes: SHL by 0 (flags unchanged), count masked mod 32/64, ROL/ROR CF and count-1 OF.
#include <stdio.h>
#include <stdint.h>
#define TGT

// defined-flag mask for a SHIFT of masked count cnt (0 => all unchanged; else CF|PF|ZF|SF, +OF iff cnt==1)
static unsigned shmask(unsigned cnt){ return cnt==0 ? 0x8D5u : (0xC5u | (cnt==1?0x800u:0u)); }
// defined-flag mask for a ROTATE of masked count cnt (rotates only set CF, and OF iff cnt==1)
static unsigned romask(unsigned cnt){ return 0x01u | (cnt==1?0x800u:0u); }

#if defined(__x86_64__)
static unsigned shl32(uint32_t v,uint8_t c,unsigned in_fl,uint32_t*res){
  uint64_t f; uint32_t r=v;
  __asm__ volatile("push %3\n\t popfq\n\t shl %%cl,%0\n\t pushfq\n\t pop %1"
                   :"+r"(r),"=r"(f):"c"(c),"r"((uint64_t)in_fl):"cc");
  *res=r; return (unsigned)f & shmask(c&31);
}
static unsigned rol32(uint32_t v,uint8_t c,uint32_t*res){
  uint64_t f; uint32_t r=v; __asm__ volatile("rol %%cl,%0\n\t pushfq\n\t pop %1":"+r"(r),"=r"(f):"c"(c):"cc");
  *res=r; return (unsigned)f & romask(c&31);
}
static unsigned ror64(uint64_t v,uint8_t c,uint64_t*res){
  uint64_t f,r=v; __asm__ volatile("ror %%cl,%0\n\t pushfq\n\t pop %1":"+r"(r),"=r"(f):"c"(c):"cc");
  *res=r; return (unsigned)f & romask(c&63);
}
#else
static unsigned shl32(uint32_t v,uint8_t c,unsigned in_fl,uint32_t*res){
  unsigned cnt=c&31;
  if(cnt==0){ *res=v; return in_fl & shmask(0); } // count 0: all flags unchanged
  uint32_t r=v<<cnt; unsigned f=0;
  if((v>>(32-cnt))&1) f|=0x01;                     // CF = last bit shifted out
  if(__builtin_parity(r&0xFF)==0) f|=0x04;         // PF
  if(r==0) f|=0x40;                                // ZF
  if(r&0x80000000u) f|=0x80;                       // SF
  if(cnt==1 && (((r>>31)&1)^(f&1))) f|=0x800;      // OF (count 1) = MSB(result) XOR CF
  *res=r; return f & shmask(cnt);
}
static unsigned rol32(uint32_t v,uint8_t c,uint32_t*res){
  unsigned cnt=c&31; if(cnt==0){ *res=v; return 0; }
  uint32_t r=(v<<cnt)|(v>>(32-cnt)); unsigned f=(r&1);         // CF = new LSB
  if(cnt==1 && (((r>>31)&1)^(r&1))) f|=0x800;                  // OF = MSB XOR CF
  *res=r; return f & romask(cnt);
}
static unsigned ror64(uint64_t v,uint8_t c,uint64_t*res){
  unsigned cnt=c&63; if(cnt==0){ *res=v; return 0; }
  uint64_t r=(v>>cnt)|(v<<(64-cnt)); unsigned f=(unsigned)((r>>63)&1); // CF = new MSB
  if(cnt==1 && (((r>>63)&1)^((r>>62)&1))) f|=0x800;                   // OF = top two bits XOR
  *res=r; return f & romask(cnt);
}
#endif

TGT int main(void){
  uint64_t h=0, seed=0xA5A5A5A55A5A5A5AULL;
  for(int it=0; it<6000; it++){
    seed = seed*6364136223846793005ULL + 1;
    unsigned inf=(unsigned)(seed&0x8D5);
    uint32_t s; unsigned f=shl32((uint32_t)seed,(uint8_t)(seed>>8),inf,&s);   // count incl. 0
    h=h*1000003ULL + s + f;
    uint32_t rl; f=rol32((uint32_t)(seed>>3),(uint8_t)(((seed>>11)%31)+1),&rl);
    h=h*1000003ULL + rl + f;
    uint64_t rr; f=ror64(seed,(uint8_t)(((seed>>17)%63)+1),&rr);
    h=h*1000003ULL + (uint32_t)rr + (uint32_t)(rr>>32) + f;
  }
  printf("shiftrot=%016llx\n",(unsigned long long)h);
  return 0;
}
