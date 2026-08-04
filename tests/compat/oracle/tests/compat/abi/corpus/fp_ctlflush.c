// Guest FP control-register behavior that a DBT must honor: flush-to-zero (x86 MXCSR
// FTZ/DAZ, aarch64 FPCR.FZ) must actually flush subnormal inputs/results, survive a
// save/restore of the control word, and the signaling ordered compare (x86 COMISS,
// aarch64 FCMPE) must raise Invalid on a qNaN operand where the quiet compare (UCOMISS,
// FCMP) does not. Each arch drives its own control register; output is normalized so the
// golden is byte-identical across ISAs. A translator that ignores the guest control
// register (silent numeric miscompile) fails this test.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
static uint32_t f2u(float f){ uint32_t u; memcpy(&u,&f,4); return u; }
static float u2f(uint32_t u){ float f; memcpy(&f,&u,4); return f; }

// Per-arch primitives: enable/disable flush, run min*0.5, tiny+tiny, save/restore ctlword,
// and quiet/signaling qNaN compares reporting whether Invalid was raised.
#if defined(__x86_64__)
static uint32_t rd(void){ uint32_t m; __asm__ volatile("stmxcsr %0":"=m"(m)); return m; }
static void wr(uint32_t m){ __asm__ volatile("ldmxcsr %0"::"m"(m)); }
#define FLUSHBITS ((1u<<15)|(1u<<6)) /* FTZ|DAZ */
#define EXCMASK 0x3f
#define UNDERFLOW (1u<<4)            /* MXCSR UE */
static int quiet_inv(float a, float b){
  wr(rd()&~EXCMASK); int r; __asm__ volatile("ucomiss %2,%1":"=@ccae"(r):"x"(a),"x"(b):"cc"); (void)r;
  return (rd()&1)!=0;
}
static int signaling_inv(float a, float b){
  wr(rd()&~EXCMASK); int r; __asm__ volatile("comiss %2,%1":"=@ccae"(r):"x"(a),"x"(b):"cc"); (void)r;
  return (rd()&1)!=0;
}
#elif defined(__aarch64__)
static uint32_t rd(void){ uint64_t v; __asm__ volatile("mrs %0, fpcr":"=r"(v)); return (uint32_t)v; }
static void wr(uint32_t m){ uint64_t v=m; __asm__ volatile("msr fpcr, %0"::"r"(v)); __asm__ volatile("isb"); }
static uint32_t rd_fpsr(void){ uint64_t v; __asm__ volatile("mrs %0, fpsr":"=r"(v)); return (uint32_t)v; }
static void wr_fpsr(uint32_t m){ uint64_t v=m; __asm__ volatile("msr fpsr, %0"::"r"(v)); }
#define FLUSHBITS (1u<<24)           /* FPCR.FZ */
#define EXCMASK 0x9f
#define UNDERFLOW (1u<<3)            /* FPSR UFC */
static int quiet_inv(float a, float b){
  wr_fpsr(rd_fpsr()&~EXCMASK);
  __asm__ volatile("fcmp %s0, %s1"::"w"(a),"w"(b):"cc");
  return (rd_fpsr()&1)!=0;
}
static int signaling_inv(float a, float b){
  wr_fpsr(rd_fpsr()&~EXCMASK);
  __asm__ volatile("fcmpe %s0, %s1"::"w"(a),"w"(b):"cc");
  return (rd_fpsr()&1)!=0;
}
#endif

#if defined(__x86_64__)
static uint32_t exc_clear_and_read_after(volatile float *out){
  wr(rd()&~EXCMASK);
  volatile float minf=u2f(0x00800000u); *out=minf*0.5f;
  return rd()&EXCMASK;
}
#elif defined(__aarch64__)
static uint32_t exc_clear_and_read_after(volatile float *out){
  wr_fpsr(rd_fpsr()&~EXCMASK);
  volatile float minf=u2f(0x00800000u); *out=minf*0.5f;
  return rd_fpsr()&EXCMASK;
}
#endif

int main(void){
  uint32_t save=rd();
  volatile float r; uint32_t e;

  puts("flush-to-zero:");
  // flush OFF: FLT_MIN*0.5 keeps the subnormal 0x00400000
  wr(save&~FLUSHBITS);
  exc_clear_and_read_after(&r);
  printf("  noflush min*0.5 = %08x\n", f2u(r));
  // flush ON: result flushed to +0, underflow raised
  wr(save|FLUSHBITS);
  e=exc_clear_and_read_after(&r);
  printf("  flush   min*0.5 = %08x underflow=%d\n", f2u(r), (e&UNDERFLOW)!=0);
  // flush ON: subnormal inputs flushed -> tiny+tiny = 0
  wr(save|FLUSHBITS);
  { volatile float t=u2f(0x00000001u); volatile float s=t+t;
    printf("  flush   tiny+tiny = %08x\n", f2u(s)); }
  // save the flush-enabled control word, disable flush, restore -> flush active again
  wr(save|FLUSHBITS);
  { uint32_t saved=rd(); wr(save&~FLUSHBITS); wr(saved);
    exc_clear_and_read_after(&r);
    printf("  roundtrip preserves flush = %d\n", f2u(r)==0); }
  wr(save);

  puts("nan-signaling:");
  float qnan=u2f(0x7fc00000u), one=1.0f;
  printf("  quiet     compare qNaN invalid=%d\n", quiet_inv(one,qnan));
  printf("  signaling compare qNaN invalid=%d\n", signaling_inv(one,qnan));
  wr(save);
  return 0;
}
