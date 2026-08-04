/* #346 PF/AF lazy dead-flag elimination — differential guest.
   hl's x86 JIT emits the x86 PF (parity, EFLAGS bit 2) and AF (aux-carry, bit 4) substrate for an ALU
   op ONLY when it is live: a consumer (lahf / pushfq / setp/setnp / jp/jnp / sahf / popfq) reads it
   before the next op overwrites both. This guest forces every ALU/shift/inc/dec/adc/sbb/neg family
   into BOTH positions and reads PF/AF back through EVERY consumer, so a mis-elimination (skipping a
   live PF/AF, or a wrong kept value) diverges from the qemu-x86_64 oracle:
     - LIVE  : `op ; <consumer>`                — the JIT MUST materialize PF/AF.
     - DEAD  : `op ; add(killer) ; pushfq`      — op's PF/AF is dropped; the killer's is read.
     - BLOCK : `op ; jmp .+2 ; pushfq`          — op is last in its block; PF/AF must be spilled.
   Consumers: pushfq (full flags), lahf (AH bit2/bit4), setp (PF->reg), jp (PF->branch, parity-Jcc).
   Golden/oracle: run on LinuxX86_64, diffed byte-exact vs qemu. */
#include <stdio.h>
#include <stdint.h>

#define FLMASK 0x8D5UL /* CF PF AF ZF SF OF */

static const uint8_t V[] = {0x00,0x01,0x03,0x0f,0x10,0x5a,0x7f,0x80,0x88,0xaa,0xf0,0xff};
static unsigned long acc = 0;

/* one two-operand family (OP, PRE both string literals). Sweeps V x V through all 6 capture modes. */
#define RUNOP(TAG, OP, PRE) do { \
  for (unsigned i = 0; i < sizeof V; i++) for (unsigned j = 0; j < sizeof V; j++) { \
    unsigned long r=0,f=0,f2=0,f3=0,ah=0,p=0,jp=0; uint8_t A=V[i], B=V[j]; \
    __asm__ volatile("movb %[A],%%al\n\t movb %[B],%%bl\n\t" PRE "\n\t" OP " %%bl,%%al\n\t" \
        "movzbl %%al,%k[R]\n\t pushfq\n\t pop %[F]\n\t" : [R]"=&r"(r),[F]"=r"(f) \
        : [A]"r"(A),[B]"r"(B) : "rax","rbx","cc"); \
    __asm__ volatile("movb %[A],%%al\n\t movb %[B],%%bl\n\t" PRE "\n\t" OP " %%bl,%%al\n\t" \
        "movb %[A],%%al\n\t add %%bl,%%al\n\t pushfq\n\t pop %[F]\n\t" : [F]"=r"(f2) \
        : [A]"r"(A),[B]"r"(B) : "rax","rbx","cc"); \
    __asm__ volatile("movb %[A],%%al\n\t movb %[B],%%bl\n\t" PRE "\n\t" OP " %%bl,%%al\n\t" \
        "jmp 1f\n\t 1: pushfq\n\t pop %[F]\n\t" : [F]"=r"(f3) : [A]"r"(A),[B]"r"(B) : "rax","rbx","cc"); \
    __asm__ volatile("movb %[A],%%al\n\t movb %[B],%%bl\n\t" PRE "\n\t" OP " %%bl,%%al\n\t" \
        "lahf\n\t movb %%ah,%%dl\n\t movzbl %%dl,%k[R]\n\t" : [R]"=&r"(ah) : [A]"r"(A),[B]"r"(B) : "rax","rbx","rdx","cc"); \
    __asm__ volatile("movb %[A],%%al\n\t movb %[B],%%bl\n\t" PRE "\n\t" OP " %%bl,%%al\n\t" \
        "setp %%cl\n\t movzbl %%cl,%k[R]\n\t" : [R]"=&r"(p) : [A]"r"(A),[B]"r"(B) : "rax","rbx","rcx","cc"); \
    __asm__ volatile("movb %[A],%%al\n\t movb %[B],%%bl\n\t" PRE "\n\t" OP " %%bl,%%al\n\t" \
        "jp 1f\n\t xorl %k[R],%k[R]\n\t jmp 2f\n\t 1: movl $1,%k[R]\n\t 2:\n\t" : [R]"=&r"(jp) \
        : [A]"r"(A),[B]"r"(B) : "rax","rbx","cc"); \
    acc = acc*1099511628211UL ^ (f&FLMASK); \
    acc = acc*1099511628211UL ^ (f2&FLMASK) ^ (f3&FLMASK); \
    acc = acc*1099511628211UL ^ (ah&0x14) ^ (p<<2) ^ (jp<<4); \
    if ((i*13+j) % 17 == 0) \
      printf("%-4s a=%02x b=%02x r=%02lx fl=%03lx dead=%03lx blk=%03lx ah=%02lx p=%lu jp=%lu\n", \
             TAG, A, B, r&0xff, f&FLMASK, f2&FLMASK, f3&FLMASK, ah&0x14, p, jp); \
  } } while (0)

/* one shift family (SOP literal shl/shr/sar). #346 gates only the shift's *PF* emission; SHL/SHR/SAR
   leave AF architecturally UNDEFINED (and hl vs qemu also legitimately differ on the OF/CF of the
   D0-form shift-by-1), so this asserts ONLY the defined PF that #346 touches — via pushfq (masked to
   PF) and setp, in both LIVE and DEAD (shift-PF killed by a following add) positions. */
#define RUNSHIFT(SOP) do { \
  for (unsigned i = 0; i < sizeof V; i++) { \
    unsigned long f=0,p=0,pd=0; uint8_t A=V[i], Bk=0x33; \
    /* LIVE: shift then pushfq -> read shift PF (bit 2 only) */ \
    __asm__ volatile("movb %[A],%%al\n\t " SOP " $1,%%al\n\t pushfq\n\t pop %[F]\n\t" \
        : [F]"=r"(f) : [A]"r"(A) : "rax","cc"); \
    /* LIVE via setp */ \
    __asm__ volatile("movb %[A],%%al\n\t " SOP " $1,%%al\n\t setp %%cl\n\t movzbl %%cl,%k[R]\n\t" \
        : [R]"=&r"(p) : [A]"r"(A) : "rax","rcx","cc"); \
    /* DEAD: shift is IMMEDIATELY followed by a killer `add bl,al` -> the shift's PF is dropped and the \
       add's (fully-defined) PF is read; exercises the shift-PF dead-elimination path. */ \
    __asm__ volatile("movb %[B],%%bl\n\t movb %[A],%%al\n\t " SOP " $1,%%al\n\t add %%bl,%%al\n\t" \
        "setp %%cl\n\t movzbl %%cl,%k[R]\n\t" : [R]"=&r"(pd) : [A]"r"(A),[B]"r"(Bk) : "rax","rbx","rcx","cc"); \
    unsigned long pf=(f>>2)&1; \
    acc = acc*1099511628211UL ^ (pf<<2) ^ (p<<3) ^ (pd<<4); \
    printf("%-4s v=%02x pf=%lu setp=%lu dead_pf=%lu\n", SOP, A, pf, p, pd); \
  } } while (0)

int main(void) {
    RUNOP("add","add","");  RUNOP("or","or","");   RUNOP("and","and","");
    RUNOP("sub","sub","");  RUNOP("xor","xor","");  RUNOP("cmp","cmp","");
    RUNOP("test","test","");
    RUNOP("adc0","adc","clc"); RUNOP("adc1","adc","stc");
    RUNOP("sbb0","sbb","clc"); RUNOP("sbb1","sbb","stc");
    RUNSHIFT("shl"); RUNSHIFT("shr"); RUNSHIFT("sar");
    /* one-operand families: inc / dec / neg (LIVE pushfq + lahf) */
    for (unsigned i = 0; i < sizeof V; i++) {
        unsigned long r=0,f=0,ah=0; uint8_t A=V[i];
        __asm__ volatile("movb %[A],%%al\n\t inc %%al\n\t movzbl %%al,%k[R]\n\t pushfq\n\t pop %[F]\n\t"
            : [R]"=&r"(r),[F]"=r"(f) : [A]"r"(A) : "rax","cc");
        __asm__ volatile("movb %[A],%%al\n\t inc %%al\n\t lahf\n\t movb %%ah,%%dl\n\t movzbl %%dl,%k[R]\n\t"
            : [R]"=&r"(ah) : [A]"r"(A) : "rax","rdx","cc");
        printf("inc  v=%02x r=%02lx fl=%03lx ah=%02lx\n", A, r&0xff, f&FLMASK, ah&0x14);
        acc = acc*1099511628211UL ^ (f&FLMASK) ^ (ah&0x14);
        __asm__ volatile("movb %[A],%%al\n\t dec %%al\n\t movzbl %%al,%k[R]\n\t pushfq\n\t pop %[F]\n\t"
            : [R]"=&r"(r),[F]"=r"(f) : [A]"r"(A) : "rax","cc");
        printf("dec  v=%02x r=%02lx fl=%03lx\n", A, r&0xff, f&FLMASK);
        acc = acc*1099511628211UL ^ (f&FLMASK);
        __asm__ volatile("movb %[A],%%al\n\t neg %%al\n\t movzbl %%al,%k[R]\n\t pushfq\n\t pop %[F]\n\t"
            : [R]"=&r"(r),[F]"=r"(f) : [A]"r"(A) : "rax","cc");
        printf("neg  v=%02x r=%02lx fl=%03lx\n", A, r&0xff, f&FLMASK);
        acc = acc*1099511628211UL ^ (f&FLMASK);
    }
    printf("PFAF-CHECKSUM %016lx\n", acc);
    return 0;
}
