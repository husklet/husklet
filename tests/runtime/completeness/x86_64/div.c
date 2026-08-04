/* x86 DIV/IDIV (group F6/F7 /6 /7) at every width: 8-bit (AX/r8), 16-bit (DX:AX/r16),
   32-bit (EDX:EAX/r32) and 64-bit (RDX:RAX/r64). These are double-width-dividend divides
   that ARM has no single instruction for; the engine fast-paths the common 64/64 case with a
   hardware UDIV/SDIV and must EXACTLY reproduce x86 quotient+remainder AND the #DE (SIGFPE)
   trap on divide-by-zero and quotient overflow (e.g. IDIV INT_MIN/-1, or a DIV whose high half
   >= the divisor). Run on LinuxX86_64, oracle-diffed vs qemu: any wrong quotient/remainder, a
   missing/extra SIGFPE, or a diverging exit code is caught byte-for-byte. A caught SIGFPE
   siglongjmps back and prints "#DE" so both normal and trapping cases stay on stdout. */
#include <stdio.h>
#include <stdint.h>
#include <setjmp.h>
#include <signal.h>

typedef unsigned long long ull;
static sigjmp_buf jb;
static volatile sig_atomic_t fpe;
static void on_fpe(int sig) { (void)sig; fpe = 1; siglongjmp(jb, 1); }

/* 8-bit: AX / r8 -> AL=quot, AH=rem. divb/idivb leave (AH<<8)|AL in AX. */
#define DIV8(ax, d, sgn) do {                                                              \
    uint16_t res = 0; fpe = 0;                                                             \
    uint16_t _a = (uint16_t)(ax); uint8_t _d = (uint8_t)(d);                               \
    if (!sigsetjmp(jb, 1)) {                                                               \
        if (sgn) __asm__ volatile("idivb %[dv]" : "=a"(res) : "a"(_a), [dv]"r"(_d) : "cc");\
        else     __asm__ volatile("divb  %[dv]" : "=a"(res) : "a"(_a), [dv]"r"(_d) : "cc");\
    }                                                                                      \
    if (fpe) printf("%s8  ax=%04x d=%02x -> #DE\n", sgn?"IDIV":"DIV ", (unsigned)_a, _d);  \
    else printf("%s8  ax=%04x d=%02x -> q=%02x r=%02x\n", sgn?"IDIV":"DIV ",               \
        (unsigned)_a, _d, res & 0xff, (res >> 8) & 0xff);                                  \
} while (0)

/* 16-bit: DX:AX / r16 -> AX=quot, DX=rem. */
#define DIV16(hi, lo, d, sgn) do {                                                                  \
    uint16_t q = 0, r = 0; fpe = 0;                                                                 \
    uint16_t _hi=(uint16_t)(hi), _lo=(uint16_t)(lo), _d=(uint16_t)(d);                              \
    if (!sigsetjmp(jb, 1)) {                                                                        \
        if (sgn) __asm__ volatile("idivw %[dv]" : "=a"(q),"=d"(r) : "a"(_lo),"d"(_hi),[dv]"r"(_d) : "cc"); \
        else     __asm__ volatile("divw  %[dv]" : "=a"(q),"=d"(r) : "a"(_lo),"d"(_hi),[dv]"r"(_d) : "cc"); \
    }                                                                                               \
    if (fpe) printf("%s16 %04x:%04x / %04x -> #DE\n", sgn?"IDIV":"DIV ", _hi,_lo,_d);               \
    else printf("%s16 %04x:%04x / %04x -> q=%04x r=%04x\n", sgn?"IDIV":"DIV ", _hi,_lo,_d,q,r);     \
} while (0)

/* 32-bit: EDX:EAX / r32 -> EAX=quot, EDX=rem. */
#define DIV32(hi, lo, d, sgn) do {                                                                  \
    uint32_t q = 0, r = 0; fpe = 0;                                                                 \
    uint32_t _hi=(uint32_t)(hi), _lo=(uint32_t)(lo), _d=(uint32_t)(d);                              \
    if (!sigsetjmp(jb, 1)) {                                                                        \
        if (sgn) __asm__ volatile("idivl %[dv]" : "=a"(q),"=d"(r) : "a"(_lo),"d"(_hi),[dv]"r"(_d) : "cc"); \
        else     __asm__ volatile("divl  %[dv]" : "=a"(q),"=d"(r) : "a"(_lo),"d"(_hi),[dv]"r"(_d) : "cc"); \
    }                                                                                               \
    if (fpe) printf("%s32 %08x:%08x / %08x -> #DE\n", sgn?"IDIV":"DIV ", _hi,_lo,_d);               \
    else printf("%s32 %08x:%08x / %08x -> q=%08x r=%08x\n", sgn?"IDIV":"DIV ", _hi,_lo,_d,q,r);     \
} while (0)

/* 64-bit: RDX:RAX / r64 -> RAX=quot, RDX=rem. */
#define DIV64(hi, lo, d, sgn) do {                                                                  \
    uint64_t q = 0, r = 0; fpe = 0;                                                                 \
    uint64_t _hi=(uint64_t)(hi), _lo=(uint64_t)(lo), _d=(uint64_t)(d);                              \
    if (!sigsetjmp(jb, 1)) {                                                                        \
        if (sgn) __asm__ volatile("idivq %[dv]" : "=a"(q),"=d"(r) : "a"(_lo),"d"(_hi),[dv]"r"(_d) : "cc"); \
        else     __asm__ volatile("divq  %[dv]" : "=a"(q),"=d"(r) : "a"(_lo),"d"(_hi),[dv]"r"(_d) : "cc"); \
    }                                                                                               \
    if (fpe) printf("%s64 %016llx:%016llx / %016llx -> #DE\n", sgn?"IDIV":"DIV ",(ull)_hi,(ull)_lo,(ull)_d);        \
    else printf("%s64 %016llx:%016llx / %016llx -> q=%016llx r=%016llx\n", sgn?"IDIV":"DIV ",                       \
        (ull)_hi,(ull)_lo,(ull)_d,(ull)q,(ull)r);                                                   \
} while (0)

int main(void) {
    struct sigaction sa; sa.sa_handler = on_fpe; sa.sa_flags = 0; sigemptyset(&sa.sa_mask);
    sigaction(SIGFPE, &sa, 0);

    /* ---- 8-bit ---- */
    DIV8(0x00ff, 0x10, 0);          /* 255/16 -> q=0x0f r=0x0f */
    DIV8(0x0080, 0x03, 0);          /* 128/3  -> q=0x2a r=0x02 */
    DIV8(0x1000, 0x01, 0);          /* quotient 0x1000 > 0xff -> #DE overflow */
    DIV8(0x0005, 0x00, 0);          /* /0 -> #DE */
    DIV8(0xff9c, 0x07, 1);          /* -100/7  -> q=-14(0xf2) r=-2(0xfe) */
    DIV8(0x0064, 0xf9, 1);          /* 100/-7  -> q=-14 r=+2 */
    DIV8(0xff80, 0xff, 1);          /* -128/-1 -> +128 > 127 -> #DE overflow */
    DIV8(0x007f, 0x01, 1);          /* 127/1   -> q=0x7f r=0 */

    /* ---- 16-bit ---- */
    DIV16(0x0000, 0xffff, 0x0010, 0);   /* 65535/16 */
    DIV16(0x0001, 0x0000, 0x0003, 0);   /* 0x10000/3 */
    DIV16(0x0010, 0x0000, 0x0001, 0);   /* DX>=divisor -> #DE overflow */
    DIV16(0x0000, 0x0064, 0x0000, 0);   /* /0 -> #DE */
    DIV16(0xffff, 0xff9c, 0x0007, 1);   /* -100/7 signed */
    DIV16(0x8000, 0x0000, 0xffff, 1);   /* INT_MIN/-1 -> #DE overflow */
    DIV16(0x0000, 0x7fff, 0x0002, 1);   /* 32767/2 */

    /* ---- 32-bit ---- */
    DIV32(0x00000000u, 0xffffffffu, 0x00000010u, 0); /* 4294967295/16 */
    DIV32(0x00000002u, 0x00000000u, 0x00000003u, 0); /* 0x2_00000000/3 */
    DIV32(0x00000005u, 0x00000000u, 0x00000001u, 0); /* EDX>=divisor -> #DE */
    DIV32(0x00000000u, 0x000186a0u, 0x00000000u, 0); /* /0 -> #DE */
    DIV32(0xffffffffu, 0xffffff9cu, 0x00000007u, 1); /* -100/7 */
    DIV32(0x80000000u, 0x00000000u, 0xffffffffu, 1); /* INT_MIN/-1 -> #DE */
    DIV32(0x00000000u, 0x7fffffffu, 0x00000002u, 1); /* INT32_MAX/2 */

    /* ---- 64-bit ---- */
    DIV64(0, 0xdeadbeefcafef00dull, 0x1234567ull, 0);        /* RDX==0 fast path */
    DIV64(0, 0xffffffffffffffffull, 0x100000000ull, 0);      /* UINT64_MAX / 2^32 */
    DIV64(0, 0x8000000000000000ull, 3ull, 0);                /* top bit set, RDX==0 */
    DIV64(1, 0, 3ull, 0);                                    /* RDX!=0 true 128/64 (2^64)/3 */
    DIV64(5, 0, 3ull, 0);                                    /* RDX(5)>=divisor(3) -> #DE overflow */
    DIV64(0, 0x123456789abcdefull, 0ull, 0);                 /* /0 -> #DE */
    DIV64(0xffffffffffffffffull, 0xffffffffffffff9cull, 7ull, 1); /* -100/7 signed, RDX==sign */
    DIV64(0, 100ull, 0xfffffffffffffff9ull, 1);              /* 100/-7 signed */
    DIV64(0, 0x8000000000000000ull, 2ull, 1);               /* +2^63 (RDX==0, RAX msb) /2 -> slow path */
    DIV64(0xffffffffffffffffull, 0x8000000000000000ull, 0xffffffffffffffffull, 1); /* INT64_MIN/-1 -> #DE */
    DIV64(0x7fffffffffffffffull, 0xffffffffffffffffull, 0x0000000100000000ull, 1); /* big 128-bit signed */
    DIV64(0, 0x7fffffffffffffffull, 1ull, 1);                /* INT64_MAX/1 */

    printf("div done\n");
    return 0;
}
