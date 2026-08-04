// Sign-extend accumulator CQO + MOVSXD + signed/unsigned 64/32 DIV/IDIV exact edges + IMUL overflow.
// (CWD 16-bit sign-extend is isolated in x_cwd — it mistranslates on the x86 engine.)
#include <stdio.h>
#include <stdint.h>
#define TGT

#if defined(__x86_64__)
// idiv 64/32-bit: dividend RDX:RAX / src -> quotient RAX, remainder RDX. cqo produces RDX sign-extend.
static void idiv64(int64_t a, int64_t b, int64_t*q, int64_t*r){
  int64_t qq,rr;
  __asm__ volatile("cqo\n\t idivq %3":"=a"(qq),"=&d"(rr):"a"(a),"r"(b):"cc");
  *q=qq;*r=rr;
}
static void div32(uint32_t a, uint32_t b, uint32_t*q, uint32_t*r){
  uint32_t qq,rr,zero=0;
  __asm__ volatile("divl %3":"=a"(qq),"=d"(rr):"a"(a),"r"(b),"d"(zero):"cc");
  *q=qq;*r=rr;
}
static int32_t movsxd(int32_t v){ int64_t r; __asm__ volatile("movslq %1,%0":"=r"(r):"r"(v)); return (int32_t)(r>>1); }
#else
static void idiv64(int64_t a, int64_t b, int64_t*q, int64_t*r){ *q=a/b; *r=a%b; }
static void div32(uint32_t a, uint32_t b, uint32_t*q, uint32_t*r){ *q=a/b; *r=a%b; }
static int32_t movsxd(int32_t v){ int64_t r=(int64_t)v; return (int32_t)(r>>1); }
#endif

TGT int main(void){
  uint64_t h=0, seed=0x7F4A7C159E3779B9ULL;
  for(int it=0; it<5000; it++){
    seed = seed*6364136223846793005ULL + 1;
    int64_t a=(int64_t)seed, b=(int64_t)(seed>>13);
    if(b==0 || b==-1) b=3; // avoid div-by-zero and INT64_MIN/-1 #DE quotient-overflow trap
    int64_t q,r; idiv64(a,b,&q,&r);
    h=h*1000003ULL + (uint64_t)q; h=h*1000003ULL + (uint64_t)r;
    uint32_t ua=(uint32_t)seed, ub=(uint32_t)(seed>>9); if(ub==0) ub=1;
    uint32_t uq,ur; div32(ua,ub,&uq,&ur);
    h=h*1000003ULL + uq; h=h*1000003ULL + ur;
    h=h*1000003ULL + (uint32_t)movsxd((int32_t)seed);
    // imul flag effect: OF/CF set when result exceeds low half — mirror via __builtin overflow
    int32_t m; int of=__builtin_mul_overflow((int32_t)seed,(int32_t)(seed>>17),&m);
    h=h*1000003ULL + (uint32_t)m + (uint32_t)of;
  }
  printf("arithext=%016llx\n",(unsigned long long)h);
  return 0;
}
