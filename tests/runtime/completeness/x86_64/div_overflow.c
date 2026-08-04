// #DE from DIV/IDIV: a zero divisor and a quotient that does not fit BOTH raise it, and Linux reports the
// SAME si_code for both -- FPE_INTDIV(1), never FPE_INTOVF(2), because the kernel classifies the trap and
// not its cause. The 64-bit forms had it wrong (si_code 2) while every narrower width had it right, which
// is the shape a guest handler that switches on si_code sees as a mis-diagnosis. Two boundary cases carry
// their own weight: RDX:RAX = INT128_MIN over -1 is the one input whose quotient overflows a 128-bit
// division too, so an engine that DIVIDES before ruling on overflow traps on its own host instruction; and
// the two largest quotients that DO fit must still divide.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static sigjmp_buf jb;
static volatile int gsig, gcode;

static void handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    gsig = sig;
    gcode = si->si_code;
    siglongjmp(jb, 1);
}

#define TRY(body)                                                                                                      \
    do {                                                                                                               \
        gsig = gcode = 0;                                                                                              \
        if (sigsetjmp(jb, 1) == 0) {                                                                                   \
            body;                                                                                                      \
        }                                                                                                              \
    } while (0)

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGFPE, &sa, NULL);

    uint64_t q, r;
    TRY(__asm__ volatile("divq %4" : "=a"(q), "=d"(r) : "0"(0ULL), "1"(1ULL), "r"(1ULL)));
    printf("div64-ovf sig=%d code=%d\n", gsig, gcode);
    TRY(__asm__ volatile("divq %4" : "=a"(q), "=d"(r) : "0"(10ULL), "1"(0ULL), "r"(0ULL)));
    printf("div64-zero sig=%d code=%d\n", gsig, gcode);
    TRY(__asm__ volatile("idivq %4"
                         : "=a"(q), "=d"(r)
                         : "0"(0x8000000000000000ULL), "1"(0xffffffffffffffffULL), "r"(0xffffffffffffffffULL)));
    printf("idiv64-minus1 sig=%d code=%d\n", gsig, gcode);
    TRY(__asm__ volatile("idivq %4" : "=a"(q), "=d"(r) : "0"(0ULL), "1"(1ULL), "r"(2ULL)));
    printf("idiv64-ovf sig=%d code=%d\n", gsig, gcode);
    TRY(__asm__ volatile("idivq %4" : "=a"(q), "=d"(r) : "0"(10ULL), "1"(0ULL), "r"(0ULL)));
    printf("idiv64-zero sig=%d code=%d\n", gsig, gcode);

    {
        uint32_t q32, r32;
        TRY(__asm__ volatile("divl %4" : "=a"(q32), "=d"(r32) : "0"(0u), "1"(1u), "r"(1u)));
        printf("div32-ovf sig=%d code=%d\n", gsig, gcode);
        TRY(__asm__ volatile("idivl %4" : "=a"(q32), "=d"(r32) : "0"(0x80000000u), "1"(0xffffffffu), "r"(0xffffffffu)));
        printf("idiv32-minus1 sig=%d code=%d\n", gsig, gcode);
        uint16_t q16, r16;
        TRY(__asm__ volatile("idivw %4" : "=a"(q16), "=d"(r16) : "0"((uint16_t)0x8000), "1"((uint16_t)0xffff),
                             "r"((uint16_t)0xffff)));
        printf("idiv16-minus1 sig=%d code=%d\n", gsig, gcode);
        TRY(__asm__ volatile("divb %2" : "=a"(q16) : "0"((uint16_t)0xff00), "q"((uint8_t)0xff)));
        printf("div8-ovf sig=%d code=%d\n", gsig, gcode);
    }

    // RDX:RAX = INT128_MIN over -1, and the same numerator plus one.
    TRY(__asm__ volatile("idivq %4"
                         : "=a"(q), "=d"(r)
                         : "0"(0ULL), "1"(0x8000000000000000ULL), "r"(0xffffffffffffffffULL)));
    printf("idiv64-int128min sig=%d code=%d\n", gsig, gcode);
    TRY(__asm__ volatile("idivq %4"
                         : "=a"(q), "=d"(r)
                         : "0"(1ULL), "1"(0x8000000000000000ULL), "r"(0xffffffffffffffffULL)));
    printf("idiv64-int128min1 sig=%d code=%d\n", gsig, gcode);

    // Boundaries that must NOT fault: the quotient is exactly INT64_MIN, resp. exactly 2^63.
    TRY(__asm__ volatile("idivq %4" : "=a"(q), "=d"(r) : "0"(0ULL), "1"(0xffffffffffffffffULL), "r"(2ULL)));
    printf("idiv64-ok sig=%d code=%d q=%016llx r=%016llx\n", gsig, gcode, (unsigned long long)q,
           (unsigned long long)r);
    TRY(__asm__ volatile("divq %4" : "=a"(q), "=d"(r) : "0"(0ULL), "1"(1ULL), "r"(2ULL)));
    printf("div64-ok sig=%d code=%d q=%016llx r=%016llx\n", gsig, gcode, (unsigned long long)q,
           (unsigned long long)r);
    TRY(__asm__ volatile("idivq %4"
                         : "=a"(q), "=d"(r)
                         : "0"(0x8000000000000000ULL), "1"(0xffffffffffffffffULL), "r"(1ULL)));
    printf("idiv64-min-by-1 sig=%d code=%d q=%016llx r=%016llx\n", gsig, gcode, (unsigned long long)q,
           (unsigned long long)r);
    TRY(__asm__ volatile("idivq %4" : "=a"(q), "=d"(r) : "0"(7ULL), "1"(0ULL), "r"(0xfffffffffffffffeULL)));
    printf("idiv64-small sig=%d code=%d q=%016llx r=%016llx\n", gsig, gcode, (unsigned long long)q,
           (unsigned long long)r);
    return 0;
}
