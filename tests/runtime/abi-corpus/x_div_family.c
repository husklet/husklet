#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <ucontext.h>

static uint64_t hash;
static volatile uintptr_t fault_pc, resume_pc;
static volatile sig_atomic_t seen_signal, seen_code, seen_exact;

static void mix(uint64_t value) { hash = (hash ^ value) * UINT64_C(0x9e3779b185ebca87); }

#define DIV_PAIR(name, type, suffix, instruction)                                                        \
    static void name##_reg(type lo, type hi, type divisor) {                                            \
        __asm__ volatile(instruction " %2" : "+a"(lo), "+d"(hi) : "c"(divisor) : "cc");             \
        mix((uint64_t)lo); mix((uint64_t)hi);                                                            \
    }                                                                                                    \
    static void name##_mem(type lo, type hi, type divisor) {                                            \
        __asm__ volatile(instruction " %2" : "+a"(lo), "+d"(hi) : "m"(divisor) : "cc");             \
        mix((uint64_t)lo); mix((uint64_t)hi);                                                            \
    }

DIV_PAIR(u16, uint16_t, w, "divw")
DIV_PAIR(s16, uint16_t, w, "idivw")
DIV_PAIR(u32, uint32_t, l, "divl")
DIV_PAIR(s32, uint32_t, l, "idivl")
DIV_PAIR(u64, uint64_t, q, "divq")
DIV_PAIR(s64, uint64_t, q, "idivq")

static void u8_reg(uint16_t ax, uint8_t divisor) {
    __asm__ volatile("divb %b1" : "+a"(ax) : "c"(divisor) : "cc");
    mix(ax & 0xff); mix(ax >> 8); /* AL quotient, AH remainder: both high-byte semantics. */
}
static void u8_mem(uint16_t ax, uint8_t divisor) {
    __asm__ volatile("divb %1" : "+a"(ax) : "m"(divisor) : "cc");
    mix(ax & 0xff); mix(ax >> 8);
}
static void s8_reg(uint16_t ax, uint8_t divisor) {
    __asm__ volatile("idivb %b1" : "+a"(ax) : "c"(divisor) : "cc");
    mix(ax & 0xff); mix(ax >> 8);
}
static void s8_mem(uint16_t ax, uint8_t divisor) {
    __asm__ volatile("idivb %1" : "+a"(ax) : "m"(divisor) : "cc");
    mix(ax & 0xff); mix(ax >> 8);
}

static void handler(int signo, siginfo_t *info, void *opaque) {
    ucontext_t *context = opaque;
    uintptr_t pc = (uintptr_t)context->uc_mcontext.gregs[REG_RIP];
    seen_signal = signo;
    seen_code = info->si_code;
    seen_exact = pc == fault_pc;
    context->uc_mcontext.gregs[REG_RIP] = (greg_t)resume_pc;
}

static void fault_reg(int signed_overflow) {
    uint64_t lo = signed_overflow ? UINT64_C(0x8000000000000000) : 7;
    uint64_t hi = signed_overflow ? UINT64_MAX : 0;
    uint64_t divisor = signed_overflow ? UINT64_MAX : 0;
    seen_signal = seen_code = seen_exact = 0;
    if (signed_overflow) {
        __asm__ volatile("leaq 1f(%%rip),%%r8; movq %%r8,%[fp]; leaq 2f(%%rip),%%r8; movq %%r8,%[rp];"
                         "1: idivq %%rcx; 2:"
                         : [fp] "=m"(fault_pc), [rp] "=m"(resume_pc), "+a"(lo), "+d"(hi)
                         : "c"(divisor) : "r8", "cc", "memory");
    } else {
        __asm__ volatile("leaq 1f(%%rip),%%r8; movq %%r8,%[fp]; leaq 2f(%%rip),%%r8; movq %%r8,%[rp];"
                         "1: divq %%rcx; 2:"
                         : [fp] "=m"(fault_pc), [rp] "=m"(resume_pc), "+a"(lo), "+d"(hi)
                         : "c"(divisor) : "r8", "cc", "memory");
    }
    mix((unsigned)seen_signal); mix((unsigned)seen_code); mix((unsigned)seen_exact);
}

static void fault_memory(void *bad) {
    uint64_t lo = 0, hi = UINT64_MAX; /* would quotient-overflow if the operand were readable and small. */
    seen_signal = seen_code = seen_exact = 0;
    __asm__ volatile("leaq 1f(%%rip),%%r8; movq %%r8,%[fp]; leaq 2f(%%rip),%%r8; movq %%r8,%[rp];"
                     "1: divq (%%rcx); 2:"
                     : [fp] "=m"(fault_pc), [rp] "=m"(resume_pc), "+a"(lo), "+d"(hi)
                     : "c"(bad) : "r8", "cc", "memory");
    mix((unsigned)seen_signal); mix((unsigned)seen_code); mix((unsigned)seen_exact);
}

static uint64_t random64(void) {
    static uint64_t state = UINT64_C(0x243f6a8885a308d3);
    state ^= state << 13; state ^= state >> 7; state ^= state << 17;
    return state;
}

int main(void) {
    struct sigaction action = {.sa_sigaction = handler, .sa_flags = SA_SIGINFO};
    sigemptyset(&action.sa_mask);
    sigaction(SIGFPE, &action, NULL);
    sigaction(SIGSEGV, &action, NULL);

    /* Boundaries: zero/high dividends, unit/max divisors, negative quotients and remainders. */
    u8_reg(0xff, 1); u8_mem(0xfe, 0xff); s8_reg((uint16_t)(int16_t)-127, (uint8_t)-3); s8_mem(127, 3);
    u16_reg(0xffff, 0, 1); u16_mem(0xfffe, 0, 0xffff); s16_reg((uint16_t)-32767, 0xffff, (uint16_t)-3); s16_mem(32767, 0, 3);
    u32_reg(UINT32_MAX, 0, 1); u32_mem(UINT32_MAX - 1, 0, UINT32_MAX); s32_reg((uint32_t)-2147483647, UINT32_MAX, (uint32_t)-3); s32_mem(INT32_MAX, 0, 3);
    u64_reg(UINT64_MAX, 0, 1); u64_mem(UINT64_MAX - 1, 0, UINT64_MAX); s64_reg((uint64_t)-INT64_MAX, UINT64_MAX, (uint64_t)-3); s64_mem(INT64_MAX, 0, 3);

    for (unsigned i = 0; i < 128; ++i) {
        uint64_t a = random64(), b = random64() | 1;
        uint8_t d8 = (uint8_t)b | 1; uint16_t ax = (uint16_t)(a % ((uint16_t)d8 << 8));
        u8_reg(ax, d8); u8_mem(ax, d8);
        int8_t sd8 = (int8_t)(d8 | 1); int16_t sn8 = (int16_t)(int8_t)a * sd8;
        s8_reg((uint16_t)sn8, (uint8_t)sd8); s8_mem((uint16_t)sn8, (uint8_t)sd8);
        uint16_t d16 = (uint16_t)b | 1, l16 = (uint16_t)a, h16 = (uint16_t)(a >> 16) % d16;
        u16_reg(l16, h16, d16); u16_mem(l16, h16, d16);
        int16_t sd16 = (int16_t)(d16 | 1), q16 = (int16_t)a / 3; int32_t n16 = (int32_t)q16 * sd16 + (sd16 / 2);
        s16_reg((uint16_t)n16, (uint16_t)((uint32_t)n16 >> 16), (uint16_t)sd16); s16_mem((uint16_t)n16, (uint16_t)((uint32_t)n16 >> 16), (uint16_t)sd16);
        uint32_t d32 = (uint32_t)b | 1, l32 = (uint32_t)a, h32 = (uint32_t)(a >> 32) % d32;
        u32_reg(l32, h32, d32); u32_mem(l32, h32, d32);
        int32_t sd32 = (int32_t)(d32 | 1), q32 = (int32_t)a / 3; int64_t n32 = (int64_t)q32 * sd32 + sd32 / 2;
        s32_reg((uint32_t)n32, (uint32_t)((uint64_t)n32 >> 32), (uint32_t)sd32); s32_mem((uint32_t)n32, (uint32_t)((uint64_t)n32 >> 32), (uint32_t)sd32);
        uint64_t d64 = b, h64 = (a >> 32) % d64;
        u64_reg(a, h64, d64); u64_mem(a, h64, d64);
        int64_t sd64 = (int64_t)(b | 1), sn64 = (int64_t)a;
        s64_reg((uint64_t)sn64, sn64 < 0 ? UINT64_MAX : 0, (uint64_t)sd64); s64_mem((uint64_t)sn64, sn64 < 0 ? UINT64_MAX : 0, (uint64_t)sd64);
    }
    fault_reg(0); fault_reg(1);
    void *bad = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (bad == MAP_FAILED) return 2;
    fault_memory(bad);
    munmap(bad, 4096);
    printf("div-family=%016llx\n", (unsigned long long)hash);
    return 0;
}
