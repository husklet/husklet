/* x86 by-CL (variable-count) shift/rotate flag materialization — differential guest (oracle: qemu-x86_64).
   Regression home for #389: a by-CL shift (group2 D2/D3 /4 SHL, /5 SHR, /7 SAR, /0 ROL, /1 ROR,
   /2 RCL, /3 RCR) must land the exact x86 CF/OF/SF/ZF/PF, and the LIVE ARM NZCV must stay canonical so a
   CHAINED (non-stitched) edge's boundary flag-spill persists the SAME value as a stitched edge. The
   original bug: `sarq %cl` (a=0, cl=1) under NOSTITCH=1 wrote CF=1/ZF=0 (should be CF=0/ZF=1) — the SAR
   by-CL path stored the right flags to cpu->nzcv but never msr'd them back to the live ARM NZCV (SHL/SHR
   re-sync inside their OF block; SAR skipped it), so the chained-edge spill persisted the stale live NZCV.

   Each case produces flags with a by-CL shift and reads them in a SEPARATE block reached by a BACKWARD
   jmp (chained edge even in the default/stitch config; and every edge is chained under NOSTITCH=1). A
   deterministic `cmp` in the producer block seeds known flags so the count==0 "flags unchanged" path is
   verifiable, and so the rotates' preserved SF/ZF/PF are deterministic. We print only bits that are x86-
   DEFINED for that (op,count) — CF/SF/ZF/PF always (defined for shifts, preserved for rotates/count==0),
   OF only for count in {0,1} (undefined for count>1) — plus the width-masked result value; a wrong flag
   or a wrong masked result diverges from qemu byte-exactly. Sweeps counts {0,1,2,7,8,31,32,63,64,65}
   (exercises count masking: &0x3f for 64-bit, &0x1f otherwise; byte/word CL=8 is a real count) over a
   range of inputs, all widths 8/16/32/64. Deterministic; oracle-diffed vs qemu-x86_64. */
#include <stdio.h>
#include <stdint.h>

/* seed operands for the producer-block cmp: cmpq $SB, $SA -> SA-SB. Fixed, deterministic flags. */
#define SA 0x5UL
#define SB 0x9UL /* 5 - 9 = -4: sets SF=1, CF=1 (borrow), OF=0, ZF=0; a known non-trivial seed */

static const uint64_t IN[] = {0, 1, 2, 0x7f, 0x80, 0xff, 0x100, 0x8000, 0xffff, 0x40000000ull,
                              0x80000000ull, 0xffffffffull, 0x8000000000000000ull,
                              0xffffffffffffffffull, 0x123456789abcdef0ull};
#define NIN (sizeof IN / sizeof IN[0])
static const unsigned CNT[] = {0, 1, 2, 7, 8, 31, 32, 63, 64, 65};
#define NCNT (sizeof CNT / sizeof CNT[0])

static unsigned long acc;

/* Emit one by-CL op: producer block seeds flags (cmp) then runs `MNE %cl, OPND`; a backward jmp reaches
   the consumer block that reads the flags via pushfq (chained edge). Returns result value + flags. */
#define RUNCL(MNE, OPND, vio, clv, of_out)                                                                \
    __asm__ volatile("movl %k[c], %%ecx\n\t"                                                               \
                     "jmp 2f\n\t"                                                                          \
                     "1: pushfq\n\t pop %[f]\n\t jmp 4f\n\t"                                               \
                     "2: cmpq %[sb], %[sa]\n\t " MNE " %%cl, " OPND "\n\t jmp 1b\n\t"                      \
                     "4:\n\t"                                                                              \
                     : [f] "=&r"(of_out), [v] "+r"(vio)                                                    \
                     : [c] "r"((unsigned)(clv)), [sa] "r"((uint64_t)SA), [sb] "r"((uint64_t)SB)           \
                     : "rcx", "cc")

/* width-defined flag bits to fold/print: CF PF ZF SF always; OF only for count in {0,1}. */
static unsigned showmask(unsigned c) {
    unsigned s = 0x01 | 0x04 | 0x40 | 0x80; /* CF PF ZF SF */
    if (c == 0 || c == 1) s |= 0x800;       /* OF defined (1-bit) or unchanged (count 0) */
    return s;
}
static uint64_t wmask(int width) { return width == 64 ? ~0UL : ((1ULL << width) - 1); }

/* fold one (op,width,count,input) outcome into acc and (sparsely) print a deterministic line. */
static void fold(const char *tag, int width, unsigned c, uint64_t vin, uint64_t res, unsigned long f) {
    uint64_t rv = res & wmask(width);
    unsigned long ff = (unsigned long)f & showmask(c);
    acc = acc * 1099511628211UL ^ rv ^ (ff << 3) ^ ((uint64_t)width << 20) ^ ((uint64_t)c << 26);
    /* print ~1 in 17 to keep stdout bounded but oracle-meaningful across every op */
    static unsigned n;
    if (n++ % 17 == 0)
        printf("%-5s w=%02d c=%02u in=%016lx r=%016lx f=%03lx\n", tag, width, c, (unsigned long)vin,
               (unsigned long)rv, ff);
}

/* run all 4 widths of one op-mnemonic-stem over every (count,input). STEM e.g. "shl","sar","rcl". */
#define OP_ALL(STEM)                                                                                      \
    do {                                                                                                  \
        for (unsigned ci = 0; ci < NCNT; ci++)                                                            \
            for (unsigned vi = 0; vi < NIN; vi++) {                                                       \
                unsigned char cl = (unsigned char)CNT[ci];                                                \
                uint64_t v;                                                                               \
                unsigned long f;                                                                          \
                v = IN[vi];                                                                               \
                RUNCL(STEM "b", "%b[v]", v, cl, f);                                                        \
                fold(STEM "b", 8, CNT[ci], IN[vi], v, f);                                                 \
                v = IN[vi];                                                                               \
                RUNCL(STEM "w", "%w[v]", v, cl, f);                                                        \
                fold(STEM "w", 16, CNT[ci], IN[vi], v, f);                                                \
                v = IN[vi];                                                                               \
                RUNCL(STEM "l", "%k[v]", v, cl, f);                                                        \
                fold(STEM "l", 32, CNT[ci], IN[vi], v, f);                                                \
                v = IN[vi];                                                                               \
                RUNCL(STEM "q", "%q[v]", v, cl, f);                                                        \
                fold(STEM "q", 64, CNT[ci], IN[vi], v, f);                                                \
            }                                                                                             \
    } while (0)

int main(void) {
    /* The exact #389 repro first, isolated and labeled, so a failure is unmistakable. */
    {
        uint64_t v = 0;
        unsigned long f = 0x99;
        RUNCL("sarq", "%q[v]", v, (unsigned char)1, f);
        /* mask CF PF ZF SF OF (AF is x86-undefined for shifts) -> want ZF|PF = 0x44 */
        printf("REPRO sarq a=0 cl=1: f=%03lx (want 044)\n", (unsigned long)f & 0x8C5);
    }

    OP_ALL("shl");
    OP_ALL("shr");
    OP_ALL("sar");
    OP_ALL("rol");
    OP_ALL("ror");
    OP_ALL("rcl");
    OP_ALL("rcr");

    printf("bycl acc=%016lx\n", acc);
    return 0;
}
