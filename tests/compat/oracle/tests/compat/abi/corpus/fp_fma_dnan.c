// FMA generated-vs-propagated NaN. When fma() internally generates a NaN (inf*0, or an
// inf-inf in the fused add) with no NaN operand, each ISA stamps its own default/indefinite
// NaN: x86 sets the sign bit (0xFFF8..), aarch64 clears it (0x7FF8..). A NaN PROPAGATED from
// an operand keeps that operand's exact bits on both ISAs. Output is normalized (compare each
// result to the running arch's own convention) so the golden is byte-identical across ISAs;
// a translator whose FMA lowering forgets the x86 indefinite sign fails this on x86.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
static uint64_t d2u(double d){ uint64_t u; memcpy(&u,&d,8); return u; }
static double u2d(uint64_t u){ double d; memcpy(&d,&u,8); return d; }

#if defined(__x86_64__)
static const uint64_t GEN_SIGN = 1;   // x86 indefinite QNaN: sign set
#else
static const uint64_t GEN_SIGN = 0;   // aarch64 default NaN: sign clear
#endif

int main(void){
  volatile double inf=INFINITY, ninf=-INFINITY, zero=0.0, one=1.0;

  // inf*0 generates a NaN in the multiply stage
  volatile double g1 = fma(inf, zero, one);
  uint64_t u1 = d2u(g1);
  printf("mul-gen  isnan=%d sign-ok=%d payload=%013llx\n",
    (g1!=g1), (int)((u1>>63)==GEN_SIGN), (unsigned long long)(u1 & 0xfffffffffffffull));

  // inf + (-inf) generates a NaN in the fused add stage (1*inf then + -inf)
  volatile double g2 = fma(one, inf, ninf);
  uint64_t u2 = d2u(g2);
  printf("add-gen  isnan=%d sign-ok=%d payload=%013llx\n",
    (g2!=g2), (int)((u2>>63)==GEN_SIGN), (unsigned long long)(u2 & 0xfffffffffffffull));

  // a qNaN operand propagates verbatim (sign + payload) on both ISAs
  volatile double qn = u2d(0x7ff8000abcde0000ull);
  volatile double p1 = fma(qn, one, one);
  printf("prop-q   keeps-input=%d\n", d2u(p1)==0x7ff8000abcde0000ull);

  // a signed qNaN operand also propagates verbatim
  volatile double qn2 = u2d(0xfff8000000000042ull);
  volatile double p2 = fma(one, one, qn2);
  printf("prop-sq  keeps-input=%d\n", d2u(p2)==0xfff8000000000042ull);
  return 0;
}
