// x87 exotic sub-forms (#248/#249): FCOMI/FUCOMI integer-flag compares, FDIV/FDIVR/FSUBR
// register forms (D8/DC/DE), FSCALE, FPREM, FIDIV integer op, FXTRACT. Verdict is a
// deterministic integer from EFLAGS bits + rounded results; diffed byte-exact vs qemu.
#include <stdio.h>
#include <stdint.h>
static int cmp_flags(double a, double b, int uc){
  uint64_t f;
  if (uc)
    __asm__ volatile("fldl %1\n\tfldl %2\n\tfucomi %%st(1),%%st\n\t"
                     "pushfq\n\tpop %0\n\tfstp %%st(0)\n\tfstp %%st(0)"
                     :"=r"(f):"m"(b),"m"(a):"cc","st","st(1)");
  else
    __asm__ volatile("fldl %1\n\tfldl %2\n\tfcomi %%st(1),%%st\n\t"
                     "pushfq\n\tpop %0\n\tfstp %%st(0)\n\tfstp %%st(0)"
                     :"=r"(f):"m"(b),"m"(a):"cc","st","st(1)");
  return (int)(f & 0x45); /* CF|PF|ZF */
}
// st0=a (%2), st1=b (%1); run mn; pop st0 -> out; drop st0.
#define OP2(mn, av, bv, out) do{ double _a=av,_b=bv; __asm__ volatile( \
    "fldl %1\n\tfldl %2\n\t" mn "\n\tfstpl %0\n\tfstp %%st(0)" \
    : "=m"(out) : "m"(_b), "m"(_a) : "st","st(1)"); }while(0)
int main(void){
  long r=0;
  r += cmp_flags(1.0, 2.0, 0);   // a<b: CF=1 -> 0x01
  r += cmp_flags(2.0, 1.0, 0)*2; // a>b: 0
  r += cmp_flags(3.0, 3.0, 0)*4; // a==b: ZF=1 -> 0x40
  r += cmp_flags(1.0, 2.0, 1)*8; // fucomi a<b
  double d1,d2,d3,d4,p,q,sig,ex;
  OP2("fdiv %%st(1),%%st",  10.0, 4.0, d1); // st0/st1 = 10/4 = 2.5
  OP2("fdivr %%st(1),%%st", 10.0, 4.0, d2); // st1/st0 = 4/10 = 0.4
  OP2("fsubr %%st(1),%%st", 10.0, 4.0, d3); // st1-st0 = 4-10 = -6
  OP2("fscale",            3.0, 2.0, d4); // 3 * 2^floor(2) = 12
  OP2("fprem",            17.0, 5.0, p);  // 17 mod 5 = 2
  r += (long)(d1*100)+(long)(d2*100)+(long)(d3*10)+(long)(d4)+(long)(p*10);
  short iv=3; double n30=30.0, n12=12.0;
  __asm__ volatile("fldl %1\n\tfidivs %2\n\tfstpl %0":"=m"(q):"m"(n30),"m"(iv):"st");
  r += (long)(q*10);
  __asm__ volatile("fldl %2\n\tfxtract\n\tfstpl %0\n\tfstpl %1"
                   :"=m"(sig),"=m"(ex):"m"(n12):"st","st(1)");
  r += (long)(ex*10) + (long)(sig*100);
  printf("x87b r=%ld\n", r);
  return 0;
}
