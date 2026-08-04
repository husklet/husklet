/* x86-64 instruction-lowering regression corpus.
 *
 * Every check below is a translator divergence that the differential ISA fuzzer
 * (tests/fuzz/isa/x86_64) found against qemu-x86_64 and that has since been fixed. The golden
 * output is the reference oracle's, so a regression in any of these lowerings shows up as an
 * exact stdout mismatch.
 *
 * Deterministic by construction: fixed inputs, no environment, no time, no addresses printed.
 * Built as x86_64-linux-gnu-gcc -O2 -static -no-pie (see the Makefile rule).
 */

#include <stdio.h>
#include <string.h>

unsigned char xmm_out[16] __attribute__((aligned(16)));
unsigned int A4[4] __attribute__((aligned(16)));
unsigned int B4[4] __attribute__((aligned(16)));
unsigned long long A2[2] __attribute__((aligned(16)));
unsigned long long B2[2] __attribute__((aligned(16)));
unsigned short mem16 = 0x1234;
unsigned char scratch[64] __attribute__((aligned(16)));

static void show(const char *tag) {
    printf("%-18s ", tag);
    for (int i = 15; i >= 0; i--)
        printf("%02x", xmm_out[i]);
    printf("\n");
}

#define VRUN(setup, body)                                                                                              \
    do {                                                                                                               \
        __asm__ volatile(setup body "\n movdqa %%xmm0, xmm_out(%%rip)" : : : "xmm0", "xmm1", "xmm2", "memory");        \
    } while (0)

#define LOAD4 "movdqa A4(%%rip), %%xmm0\n movdqa B4(%%rip), %%xmm1\n"
#define LOAD2 "movdqa A2(%%rip), %%xmm0\n movdqa B2(%%rip), %%xmm1\n"

/* ---------------------------------------------------------------- scalar SSE upper lanes */
/* CMPSS/CMPSD, the scalar arithmetic, the scalar min/max, CVTSI2SD, CVTSS2SD and CVTSD2SS all
 * write ONLY the low element; bits 127:64 (127:32 for the ss forms) are architecturally
 * preserved. The ARM scalar forms zero them, so each needed an explicit merge. */
static void scalar_upper_lanes(void) {
    A2[0] = 0x3ff0000000000000ULL;
    A2[1] = 0x1122334455667788ULL;
    B2[0] = 0x4000000000000000ULL;
    B2[1] = 0x99aabbccddeeff00ULL;
    VRUN(LOAD2, "addsd %%xmm1,%%xmm0");
    show("addsd");
    VRUN(LOAD2, "subss %%xmm1,%%xmm0");
    show("subss");
    VRUN(LOAD2, "mulss %%xmm1,%%xmm0");
    show("mulss");
    VRUN(LOAD2, "divsd %%xmm1,%%xmm0");
    show("divsd");
    VRUN(LOAD2, "sqrtsd %%xmm1,%%xmm0");
    show("sqrtsd");
    VRUN(LOAD2, "minsd %%xmm1,%%xmm0");
    show("minsd");
    VRUN(LOAD2, "maxss %%xmm1,%%xmm0");
    show("maxss");
    VRUN(LOAD2, "cmpsd $4,%%xmm1,%%xmm0");
    show("cmpsd-neq");
    VRUN(LOAD2, "cmpss $0,%%xmm1,%%xmm0");
    show("cmpss-eq");
    VRUN(LOAD2, "cvtss2sd %%xmm1,%%xmm0");
    show("cvtss2sd");
    VRUN(LOAD2, "cvtsd2ss %%xmm1,%%xmm0");
    show("cvtsd2ss");
    VRUN(LOAD2, "movq $0x123456789, %%rax\n cvtsi2sdq %%rax,%%xmm0");
    show("cvtsi2sdq");
}

/* ---------------------------------------------------------------- packed conversions */
/* Legacy (non-VEX) CVTPS2PD / CVTPD2PS used to fall into the SCALAR 0F 5A arm and produced a
 * single converted element plus garbage. */
static void packed_converts(void) {
    A2[0] = 0x4055000000000000ULL; /* 84.0            */
    A2[1] = 0xc07e000000000000ULL; /* -480.0          */
    B2[0] = 0x42c8000041200000ULL; /* floats 10.0, 100.0 */
    B2[1] = 0x4048000040000000ULL; /* floats 2.0, 3.125  */
    VRUN(LOAD2, "cvtpd2ps %%xmm0,%%xmm0");
    show("cvtpd2ps");
    VRUN(LOAD2, "cvtps2pd %%xmm1,%%xmm0");
    show("cvtps2pd");
}

/* ---------------------------------------------------------------- SSE NaN selection */
/* x86 selects SRC1 whenever SRC1 is a NaN of either kind, else SRC2, and quiets it -- plain src1
 * priority, measured on Zen 4 over all 64 ordered NaN pairs. The (QNaN, QNaN) pair -- the common
 * one, since every propagated NaN is quiet -- used to come out as SRC2. Also: horizontal ops add
 * the EVEN lane FIRST, and a GENERATED NaN carries x86's negative indefinite sign. */
static void sse_nan(void) {
    static const unsigned int NANS[][2] = {
        {0xffffffffu, 0x7fc00001u}, /* qnan , qnan */
        {0xffffffffu, 0x7f800002u}, /* qnan , snan */
        {0xff800001u, 0x7fc00001u}, /* snan , qnan */
        {0xff800001u, 0x7f800002u}, /* snan , snan */
        {0xff800001u, 0x40000000u}, /* snan , value */
        {0x40000000u, 0x7f800002u}, /* value, snan */
    };
    for (unsigned i = 0; i < sizeof NANS / sizeof NANS[0]; i++) {
        A4[0] = NANS[i][0];
        A4[1] = A4[2] = A4[3] = 0x3f800000u;
        B4[0] = NANS[i][1];
        B4[1] = B4[2] = B4[3] = 0x40000000u;
        char tag[32];
        snprintf(tag, sizeof tag, "subps-nan%u", i);
        VRUN(LOAD4, "subps %%xmm1,%%xmm0");
        show(tag);
        snprintf(tag, sizeof tag, "mulps-nan%u", i);
        VRUN(LOAD4, "mulps %%xmm1,%%xmm0");
        show(tag);
    }
    /* generated NaN (inf + -inf) must carry the x86 negative indefinite sign */
    A4[0] = 0x7f800000u;
    A4[1] = 0xff800000u;
    A4[2] = 0x7f800000u;
    A4[3] = 0x7f800000u;
    B4[0] = 0x41000000u;
    B4[1] = 0x41200000u;
    B4[2] = 0x41400000u;
    B4[3] = 0x41600000u;
    VRUN(LOAD4, "haddps %%xmm1,%%xmm0");
    show("haddps-geninf");
    VRUN(LOAD4, "hsubps %%xmm1,%%xmm0");
    show("hsubps-geninf");
    VRUN(LOAD4, "addsubps %%xmm1,%%xmm0");
    show("addsubps-geninf");
    /* addsub must not sign-flip a NaN operand on its subtracting lanes */
    A4[0] = 0x3f800000u;
    A4[1] = 0x3f800000u;
    A4[2] = 0x3f800000u;
    A4[3] = 0x3f800000u;
    B4[0] = 0x7fc00001u;
    B4[1] = 0x7fc00002u;
    B4[2] = 0x7fc00003u;
    B4[3] = 0x7fc00004u;
    VRUN(LOAD4, "addsubps %%xmm1,%%xmm0");
    show("addsubps-nan");
    A2[0] = 0x3ff0000000000000ULL;
    A2[1] = 0x3ff0000000000000ULL;
    B2[0] = 0x7ff8000000000001ULL;
    B2[1] = 0x7ff8000000000002ULL;
    VRUN(LOAD2, "addsubpd %%xmm1,%%xmm0");
    show("addsubpd-nan");
    /* horizontal add pairs (high, low) with the HIGH lane as the first operand */
    A2[0] = 0xfff8000000000000ULL;
    A2[1] = 0x7ff8000000000000ULL;
    B2[0] = 0x3ff0000000000000ULL;
    B2[1] = 0x4000000000000000ULL;
    VRUN(LOAD2, "haddpd %%xmm1,%%xmm0");
    show("haddpd-nanpair");
}

/* ---------------------------------------------------------------- comisd/ucomisd flags */
/* x86 clears OF (and SF and AF) for every (U)COMISS/(U)COMISD. ARM FCMP reports "unordered" by
 * SETTING V, which is exactly where this engine keeps x86 OF -- so an unordered compare used to
 * leave OF=1 and mis-steer jo/seto/cmovo and the signed SF!=OF conditions. */
static void comis_flags(void) {
    static const unsigned long long PAIRS[][2] = {
        {0x7ff8000000000000ULL, 0x3ff0000000000000ULL}, /* NaN vs 1.0 -> unordered */
        {0x3ff0000000000000ULL, 0x7ff8000000000000ULL}, /* 1.0 vs NaN -> unordered */
        {0x3ff0000000000000ULL, 0x4000000000000000ULL}, /* 1.0 < 2.0             */
        {0x4000000000000000ULL, 0x4000000000000000ULL}, /* equal                 */
    };
    for (unsigned i = 0; i < 4; i++) {
        unsigned long ovf = 0, flags = 0;
        A2[0] = PAIRS[i][0];
        B2[0] = PAIRS[i][1];
        __asm__ volatile("movsd A2(%%rip), %%xmm0\n movsd B2(%%rip), %%xmm1\n"
                         "ucomisd %%xmm1, %%xmm0\n"
                         "seto %%al\n movzbl %%al, %%eax\n movq %%rax, %0\n"
                         "pushfq\n popq %1"
                         : "=r"(ovf), "=r"(flags)
                         :
                         : "rax", "xmm0", "xmm1", "cc", "memory");
        printf("ucomisd%-11u of=%lu flags=%04lx\n", i, ovf, flags & 0x8d5u);
    }
}

/* ---------------------------------------------------------------- 16-bit partial writes */
/* A 16-bit destination write PRESERVES bits 63:16; only the 32-bit forms zero-extend. */
static void narrow_writes(void) {
    unsigned long r = 0, r2 = 0;
#define SEED "movabsq $0xaaaabbbbccccdddd, %%rax\n movabsq $0x5555666677778888, %%rbx\n"
#define ONE(tag, body)                                                                                                 \
    do {                                                                                                               \
        __asm__ volatile(SEED body "\n movq %%rax, %0" : "=r"(r) : : "rax", "rbx", "rcx", "cc", "memory");             \
        printf("%-18s %016lx\n", tag, r);                                                                              \
    } while (0)
    ONE("movw-mem", "movw mem16(%%rip), %%ax");
    ONE("movw-imm", "movw $0x77, %%ax");
    ONE("movw-regreg", "movw %%bx, %%ax");
    ONE("cmovew-mem", "xorl %%ecx, %%ecx\n cmovew mem16(%%rip), %%ax");
    ONE("cmovew-reg", "xorl %%ecx, %%ecx\n cmovew %%bx, %%ax");
    ONE("popcntw", "popcntw mem16(%%rip), %%ax");
    ONE("bsfw", "bsfw %%bx, %%ax");
    ONE("bsrw", "bsrw %%bx, %%ax");
    ONE("xchgw-reg", "xchgw %%bx, %%ax");
    ONE("xchgw-mem", "xchgw mem16(%%rip), %%ax");
    ONE("leaw", "leaw 4(%%rbx), %%ax");
    ONE("pushpopw", "pushq %%rbx\n popw %%ax\n addq $6, %%rsp");
    /* the 16-bit push/pop pair must move RSP by 2, not 8 */
    __asm__ volatile("movq %%rsp, %%rcx\n pushw %%bx\n popw %%dx\n subq %%rsp, %%rcx\n movq %%rcx, %0"
                     : "=r"(r2)
                     :
                     : "rbx", "rcx", "rdx", "memory");
    printf("%-18s %016lx\n", "pushw-rsp-delta", r2);
#undef ONE
}

/* ---------------------------------------------------------------- cmpxchg / xadd */
/* The 8- and 16-bit REGISTER forms used to be UNIMPL (a hard engine abort), and the flags of
 * every CMPXCHG width were computed as (dest - accumulator) instead of (accumulator - dest). */
static void cmpxchg_xadd(void) {
    unsigned long a, b, d, f;
#define CX(tag, body)                                                                                                  \
    do {                                                                                                               \
        __asm__ volatile("movabsq $0xaaaabbbbccccdddd, %%rax\n"                                                        \
                         "movabsq $0x5555666677778888, %%rbx\n"                                                        \
                         "movabsq $0x1111222233334444, %%rdx\n" body                                                   \
                         "\n movq %%rax,%0\n movq %%rbx,%1\n movq %%rdx,%2\n pushfq\n popq %3"                         \
                         : "=r"(a), "=r"(b), "=r"(d), "=r"(f)                                                          \
                         :                                                                                             \
                         : "rax", "rbx", "rdx", "cc", "memory");                                                       \
        printf("%-18s %016lx %016lx %016lx %04lx\n", tag, a, b, d, f & 0x8d5u);                                        \
    } while (0)
    CX("cmpxchgb-ne", "cmpxchgb %%bl, %%dl");
    CX("cmpxchgb-eq", "movb %%dl, %%al\n cmpxchgb %%bl, %%dl");
    CX("cmpxchgb-hi8", "cmpxchgb %%ah, %%bh");
    CX("cmpxchgw-ne", "cmpxchgw %%bx, %%dx");
    CX("cmpxchgw-eq", "movw %%dx, %%ax\n cmpxchgw %%bx, %%dx");
    /* The 32-bit REGISTER form is deliberately absent: on the failing-comparison path the SDM's
     * `DEST <- DEST` write zero-extends bits 63:32 on hardware, while qemu-user preserves them.
     * That is an oracle disagreement, not a translator property, so it is not pinned here. */
    CX("cmpxchgq", "cmpxchgq %%rbx, %%rdx");
    CX("xaddb", "xaddb %%bl, %%dl");
    CX("xaddb-hi8", "xaddb %%ah, %%bh");
    CX("xaddw", "xaddw %%bx, %%dx");
    CX("xaddw-same", "xaddw %%ax, %%ax");
    CX("xaddq", "xaddq %%rbx, %%rdx");
#undef CX
}

/* ---------------------------------------------------------------- popcnt flags */
/* POPCNT sets ZF from the SOURCE and clears OF, SF, AF, CF and PF -- all of them. */
static void popcnt_flags(void) {
    static const unsigned long SRC[] = {0, 1, 0x8000000000000000ULL, 0xffffffffffffffffULL, 0x0f0f};
    for (unsigned i = 0; i < 5; i++) {
        unsigned long np = 0, res = 0, flags = 0;
        __asm__ volatile("popcntq %3, %0\n setnp %%cl\n movzbl %%cl, %%ecx\n movq %%rcx, %1\n"
                         "pushfq\n popq %2"
                         : "=r"(res), "=r"(np), "=r"(flags)
                         : "r"(SRC[i])
                         : "rcx", "cc");
        printf("popcnt%-12u %2lu np=%lu flags=%04lx\n", i, res, np, flags & 0x8d5u);
    }
}

/* ---------------------------------------------------------------- deferred-flag clobbers */
/* None of these SSE instructions is an x86 flag producer, but each one's lowering writes the
 * host NZCV (a count clamp, a default-NaN probe, an out-of-range probe). With the guest's flags
 * still deferred in that register, the following integer consumer read manufactured flags. */
static void sse_flag_clobber(void) {
    unsigned long r = 0;
    memset(scratch, 0x5a, sizeof scratch);
    /* variable SSE shift between an 8-bit ADC and a carry consumer */
    __asm__ volatile("movabsq $0x00000000000000f0, %%rbx\n movabsq $0x0000000000000020, %%r8\n"
                     "stc\n adcb %%r8b, %%bl\n"
                     "movdqa A4(%%rip), %%xmm5\n movdqa B4(%%rip), %%xmm7\n psraw %%xmm7, %%xmm5\n"
                     "movb $8, %%cl\n rclb %%cl, %%bl\n movq %%rbx, %0"
                     : "=r"(r)
                     :
                     : "rbx", "rcx", "r8", "xmm5", "xmm7", "cc", "memory");
    printf("%-18s %016lx\n", "adc-psraw-rcl", r);
    /* scalar SSE default-NaN probe between a compare and a jcc */
    A2[0] = 0xfff0000000000000ULL;
    B2[0] = 0x7ff0000000000000ULL;
    __asm__ volatile("movq $1, %%rbx\n cmpq $2, %%rbx\n" /* SF=1 */
                     "movsd A2(%%rip), %%xmm0\n movsd B2(%%rip), %%xmm1\n addsd %%xmm1, %%xmm0\n"
                     "movq $0, %%rbx\n js 1f\n movq $7, %%rbx\n 1:\n movq %%rbx, %0"
                     : "=r"(r)
                     :
                     : "rbx", "xmm0", "xmm1", "cc", "memory");
    printf("%-18s %016lx\n", "cmp-addsd-js", r);
    /* cvttsd2si out-of-range probe between a compare and a jcc */
    A2[0] = 0x7ff8000000000000ULL;
    __asm__ volatile("movq $1, %%rbx\n cmpq $2, %%rbx\n" /* SF=1 */
                     "movsd A2(%%rip), %%xmm0\n cvttsd2si %%xmm0, %%rdi\n"
                     "movq $0, %%rbx\n js 1f\n movq $9, %%rbx\n 1:\n movq %%rbx, %0"
                     : "=r"(r)
                     :
                     : "rbx", "rdi", "xmm0", "cc", "memory");
    printf("%-18s %016lx\n", "cmp-cvttsd2si-js", r);
}

/* ---------------------------------------------------------------- unaligned (split-lock) atomics */
/* `xchg reg,mem` is implicitly locked and every `lock`-prefixed RMW is legal at ANY address on x86.
 * The backend lowers them to single LSE atomics (SWPAL / LDADDAL / LDCLRAL / LDEORAL / LDSETAL /
 * CASAL), which alignment-fault on AArch64 -- so an unaligned one used to kill the guest with
 * SIGBUS. A SIGBUS/BUS_ADRALN fixup (lse_align_fixup, linux_abi/x86.c) now emulates them. */
static unsigned char atom[256] __attribute__((aligned(64)));

static void atom_reset(void) {
    for (unsigned i = 0; i < sizeof atom; i++)
        atom[i] = (unsigned char)(i * 7 + 3);
}

static void unaligned_atomics(void) {
    for (int off = 1; off <= 15; off += 7) {
        unsigned char *p = atom + 64 + off;
        unsigned long a, m;
        atom_reset();
        a = 0x1122334455667788ULL;
        __asm__ volatile("xchgq %0,(%1)" : "+r"(a) : "r"(p) : "memory");
        memcpy(&m, p, 8);
        printf("%-18s %2d %016lx %016lx\n", "xchg64", off, a, m);
        atom_reset();
        a = 0x00000000deadbeefULL;
        __asm__ volatile("lock xaddl %k0,(%1)" : "+r"(a) : "r"(p) : "memory", "cc");
        memcpy(&m, p, 4);
        printf("%-18s %2d %016lx %016lx\n", "xadd32", off, a, m & 0xffffffffUL);
        atom_reset();
        a = 0x0f0f0f0f0f0f0f0fULL;
        __asm__ volatile("lock andq %0,(%1)" : : "r"(a), "r"(p) : "memory", "cc");
        memcpy(&m, p, 8);
        printf("%-18s %2d %016lx\n", "lock-and64", off, m);
        atom_reset();
        a = 0x1000000000000001ULL;
        __asm__ volatile("lock orq %0,(%1)" : : "r"(a), "r"(p) : "memory", "cc");
        memcpy(&m, p, 8);
        printf("%-18s %2d %016lx\n", "lock-or64", off, m);
        atom_reset();
        __asm__ volatile("lock btsq $37,(%0)" : : "r"(p) : "memory", "cc");
        memcpy(&m, p, 8);
        printf("%-18s %2d %016lx\n", "lock-bts64", off, m);
        atom_reset();
        { /* cmpxchg, both the taken and the failing comparison */
            unsigned long want = 0, rax, nv = 0xAAAABBBBCCCCDDDDULL;
            int z;
            memcpy(&want, p, 8);
            rax = want;
            __asm__ volatile("lock cmpxchgq %3,(%2)\n setz %b1"
                             : "+a"(rax), "=q"(z)
                             : "r"(p), "r"(nv)
                             : "memory", "cc");
            memcpy(&m, p, 8);
            printf("%-18s %2d z=%d %016lx %016lx\n", "cmpxchg-hit", off, z & 1, rax, m);
            rax = 0xdeadbeefdeadbeefULL;
            __asm__ volatile("lock cmpxchgq %3,(%2)\n setz %b1"
                             : "+a"(rax), "=q"(z)
                             : "r"(p), "r"(nv)
                             : "memory", "cc");
            memcpy(&m, p, 8);
            printf("%-18s %2d z=%d %016lx %016lx\n", "cmpxchg-miss", off, z & 1, rax, m);
        }
        atom_reset();
        a = 0x12345678;
        __asm__ volatile("xchgl %k0,(%1)" : "+r"(a) : "r"(p) : "memory");
        memcpy(&m, p, 4);
        printf("%-18s %2d %016lx %016lx\n", "xchg32", off, a, m & 0xffffffffUL);
    }
}

/* ---------------------------------------------------------------- two-NaN operand selection */
/* When both inputs of an FP lane are NaN, x86 keeps SRC1 unconditionally (quieted) -- not ARM's
 * SNaN-first-else-src1 rule, which agrees on 12 of these 16 pairs and diverges on the four with a
 * QNaN src1 and an SNaN src2 (see avx_dnan_f32/f64). The SSE3 horizontal/addsub family
 * (0F 7C/7D/D0) had NO NaN-input gate at all and so always took ARM's answer. */
static const unsigned long long NANK[6] = {
    0x7ff0000000000001ULL, /* sNaN + payload 1 */
    0x7ff8000000000009ULL, /* qNaN + payload 9 */
    0xfff0000000000002ULL, /* sNaN - payload 2 */
    0xfff8000000000004ULL, /* qNaN - payload 4 */
    0x7ff0000000000000ULL, /* +inf  */
    0x3ff0000000000000ULL, /* 1.0   */
};

static void nan_pairs(void) {
    for (int i = 0; i < 6; i++)
        for (int j = 0; j < 6; j++) {
            A2[0] = A2[1] = NANK[i];
            B2[0] = B2[1] = NANK[j];
            VRUN(LOAD2, "addpd %%xmm1,%%xmm0");
            printf("addpd %d%d ", i, j);
            show("");
            VRUN(LOAD2, "mulpd %%xmm1,%%xmm0");
            printf("mulpd %d%d ", i, j);
            show("");
            /* horizontal: the pair is (even,odd) WITHIN each source */
            A2[0] = NANK[i];
            A2[1] = NANK[j];
            B2[0] = NANK[j];
            B2[1] = NANK[i];
            VRUN(LOAD2, "haddpd %%xmm1,%%xmm0");
            printf("haddpd %d%d ", i, j);
            show("");
            VRUN(LOAD2, "hsubpd %%xmm1,%%xmm0");
            printf("hsubpd %d%d ", i, j);
            show("");
            VRUN(LOAD2, "addsubpd %%xmm1,%%xmm0");
            printf("addsubpd %d%d ", i, j);
            show("");
            A4[0] = (unsigned)(NANK[i] >> 32);
            A4[1] = (unsigned)(NANK[j] >> 32);
            A4[2] = (unsigned)(NANK[j] >> 32);
            A4[3] = (unsigned)(NANK[i] >> 32);
            B4[0] = (unsigned)(NANK[j] >> 32);
            B4[1] = (unsigned)(NANK[i] >> 32);
            B4[2] = (unsigned)(NANK[i] >> 32);
            B4[3] = (unsigned)(NANK[j] >> 32);
            VRUN(LOAD4, "haddps %%xmm1,%%xmm0");
            printf("haddps %d%d ", i, j);
            show("");
            VRUN(LOAD4, "hsubps %%xmm1,%%xmm0");
            printf("hsubps %d%d ", i, j);
            show("");
            VRUN(LOAD4, "addsubps %%xmm1,%%xmm0");
            printf("addsubps %d%d ", i, j);
            show("");
        }
}

/* ---------------------------------------------------------------- in-place float->int converts */
/* CVT(T)PS2DQ needs a "make indefinite" mask built from the SOURCE floats. It used to be built
 * AFTER the FCVT*S, which for the in-place `cvttps2dq %xmm0,%xmm0` form read the integer result
 * reinterpreted as f32: an all-ones lane looked like a NaN and was forced to 0x80000000, and a
 * genuine indefinite lane (0x80000000 == -0.0f) looked ordered and kept ARM's NaN->0. */
static void cvt_in_place(void) {
    static const unsigned int SRC[4] = {0xbfc00000u /* -1.5 */, 0x7fc00000u /* NaN */, 0x4f000000u /* 2^31 */,
                                        0x42c80000u /* 100.0 */};
    for (int rot = 0; rot < 4; rot++) {
        for (int i = 0; i < 4; i++)
            A4[i] = SRC[(i + rot) & 3];
        __asm__ volatile("movdqa A4(%%rip), %%xmm0\n cvttps2dq %%xmm0, %%xmm0\n"
                         " movdqa %%xmm0, xmm_out(%%rip)"
                         :
                         :
                         : "xmm0", "memory");
        printf("cvttps2dq-ip%d ", rot);
        show("");
        __asm__ volatile("movdqa A4(%%rip), %%xmm0\n cvtps2dq %%xmm0, %%xmm0\n"
                         " movdqa %%xmm0, xmm_out(%%rip)"
                         :
                         :
                         : "xmm0", "memory");
        printf("cvtps2dq-ip%d  ", rot);
        show("");
    }
}

/* ---------------------------------------------------------------- x87 generated-NaN sign */
/* An x87 invalid operation delivers the QNaN indefinite with the SIGN BIT SET; ARM's FADD/FSUB/
 * FMUL/FDIV/FSQRT deliver the positive default NaN (identical payload, opposite sign). */
static double fa, fb, fr;

static void x87_indefinite(void) {
#define X87_2(tag, x, y, ins)                                                                                          \
    do {                                                                                                               \
        fa = (x);                                                                                                      \
        fb = (y);                                                                                                      \
        __asm__ volatile("fldl %1\n fldl %2\n " ins "\n fstpl %0" : "=m"(fr) : "m"(fb), "m"(fa) : "memory", "st");     \
        unsigned long u;                                                                                               \
        memcpy(&u, &fr, 8);                                                                                            \
        printf("%-18s %016lx\n", tag, u);                                                                              \
    } while (0)
#define X87_1(tag, x, ins)                                                                                             \
    do {                                                                                                               \
        fa = (x);                                                                                                      \
        __asm__ volatile("fldl %1\n " ins "\n fstpl %0" : "=m"(fr) : "m"(fa) : "memory", "st");                        \
        unsigned long u;                                                                                               \
        memcpy(&u, &fr, 8);                                                                                            \
        printf("%-18s %016lx\n", tag, u);                                                                              \
    } while (0)
    double inf = __builtin_inf(), zero = 0.0, nnan = -__builtin_nan("");
    X87_1("fsqrt-neg", -4.0, "fsqrt");
    X87_1("fsqrt-negzero", -0.0, "fsqrt");
    X87_2("fdiv-0/0", zero, zero, "fdivp %%st,%%st(1)");
    X87_2("fdiv-inf/inf", inf, inf, "fdivp %%st,%%st(1)");
    X87_2("fsub-inf-inf", inf, inf, "fsubp %%st,%%st(1)");
    X87_2("fmul-0*inf", zero, inf, "fmulp %%st,%%st(1)");
    X87_2("fadd-inf-inf", inf, -inf, "faddp %%st,%%st(1)");
    X87_2("fdivr-0/0", zero, zero, "fdivrp %%st,%%st(1)");
    X87_2("fprem-0", -1.0, zero, "fprem\n fstp %%st(1)");
    /* FSCALE is the identity on +-inf / +-0 / NaN for EVERY scale; the biased-exponent clamp used to
     * turn those into 0*inf (an indefinite) once the scale saturated the clamp in either direction. */
    X87_2("fscale-inf-down", inf, -1.0e30, "fscale\n fstp %%st(1)");
    X87_2("fscale-ninf-down", -inf, -1.0e30, "fscale\n fstp %%st(1)");
    X87_2("fscale-zero-up", zero, 1.0e30, "fscale\n fstp %%st(1)");
    X87_2("fscale-nzero-up", -0.0, 1.0e30, "fscale\n fstp %%st(1)");
    X87_2("fscale-one-down", 1.0, -1.0e30, "fscale\n fstp %%st(1)");
    X87_2("fscale-one-up", 1.0, 1.0e30, "fscale\n fstp %%st(1)");
    X87_2("fscale-normal", 3.0, 2.0, "fscale\n fstp %%st(1)");
    X87_2("fscale-nan", nnan, 5.0, "fscale\n fstp %%st(1)");
    /* a PROPAGATED NaN keeps its own sign on both ISAs -- must NOT be stamped */
    X87_2("fadd-propagate", nnan, 1.0, "faddp %%st,%%st(1)");
    X87_2("fmul-propagate", -nnan, 1.0, "fmulp %%st,%%st(1)");
    X87_1("fsqrt-propagate", -nnan, "fsqrt");
#undef X87_1
#undef X87_2
}

/* ---------------------------------------------------------------- PCMPISTRI equal-ordered */
/* The equal-ordered aggregation (imm[3:2]=11b) walks the needle over operand2 with the SDM's
 * asymmetric validity table: needle exhausted -> forced TRUE, haystack exhausted first -> forced
 * FALSE. The inner loop used to be bounded by the needle length rather than the element count, so a
 * needle reaching the end of a null-free operand2 was wrongly reported as a mismatch. */
static const char *const PSTR[8] = {
    "abcdef\0\0\0\0\0\0\0\0\0", "abc\0defghijklmn",          "\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    "xyzabcdefghijkl",          "aaaaaaaaaaaaaaa",           "zzabcdef\0zzzzzz",
    "abcabcabcabcabc",          "nopq\0\0\0\0\0\0\0\0\0\0\0"};

static void pcmpistri_forms(void) {
    static const int IMM[10] = {0x00, 0x04, 0x08, 0x0c, 0x1c, 0x3c, 0x5c, 0x7c, 0x0d, 0x1d};
    for (int ia = 0; ia < 8; ia++)
        for (int ib = 0; ib < 8; ib++) {
            memset(A2, 0, 16);
            memset(B2, 0, 16);
            memcpy(A2, PSTR[ia], 15);
            memcpy(B2, PSTR[ib], 15);
            ((unsigned char *)A2)[15] = (ia == 4 || ia == 6) ? 'q' : 0;
            ((unsigned char *)B2)[15] = (ib == 4 || ib == 6) ? 'q' : 0;
            for (int k = 0; k < 10; k++) {
                unsigned long rcx = 0, fl = 0;
                switch (IMM[k]) {
#define PIS(im)                                                                                                        \
    case im:                                                                                                           \
        __asm__ volatile("movdqa A2(%%rip),%%xmm0\n movdqa B2(%%rip),%%xmm1\n"                                         \
                         " pcmpistri $" #im ",%%xmm1,%%xmm0\n movq %%rcx,%0\n pushfq\n popq %1"                        \
                         : "=r"(rcx), "=r"(fl)                                                                         \
                         :                                                                                             \
                         : "xmm0", "xmm1", "rcx", "cc", "memory");                                                     \
        break;
                    PIS(0x00)
                    PIS(0x04) PIS(0x08) PIS(0x0c) PIS(0x1c) PIS(0x3c) PIS(0x5c) PIS(0x7c) PIS(0x0d) PIS(0x1d)
#undef PIS
                        default : break;
                }
                printf("pcmpistri %02x %d%d %2lu %04lx\n", IMM[k], ia, ib, rcx, fl & 0x8d5UL);
            }
        }
}

int main(void) {
    scalar_upper_lanes();
    packed_converts();
    sse_nan();
    comis_flags();
    narrow_writes();
    cmpxchg_xadd();
    popcnt_flags();
    sse_flag_clobber();
    unaligned_atomics();
    nan_pairs();
    cvt_in_place();
    x87_indefinite();
    pcmpistri_forms();
    return 0;
}
