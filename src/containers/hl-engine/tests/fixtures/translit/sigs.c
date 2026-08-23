// translit/sigs -- signals into transliterated frames.
//
// A fault inside emitted code is reconstructed by translit_signal_capture: guest GPRs are the host GPRs,
// so *cpu comes straight out of the ucontext. Three shapes: a SIGSEGV raised from the middle of a long
// straight-line run with every host register holding a guest value, a SIGFPE from a class the
// transliterator refuses (div), and a guest stack overflow -- which is the case SA_ONSTACK exists for,
// because the transliterator's HOST stack IS the guest stack and an overflowed guest stack leaves no room
// for the host signal frame.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <signal.h>
#include <setjmp.h>
#include <unistd.h>
#include <sys/mman.h>
static sigjmp_buf pad;
static volatile long caught, last_sig, last_code;

static void handler(int sig, siginfo_t *si, void *uc) {
    caught++;
    last_sig = sig;
    last_code = si->si_code;
    siglongjmp(pad, 1);
}

static volatile long spin;

// A fault raised from deep inside a straight-line run of transliterable
// instructions: every host GPR is holding a guest value when the signal lands.
__attribute__((noinline)) static long faulting(volatile long *p, long a, long b, long c, long d) {
    long r = a * 3 + b * 5 + c * 7 + d * 11;
    r ^= (r << 13);
    r ^= (r >> 7);
    r += a + b;
    r ^= *p; // <- the fault
    r ^= (r << 17);
    return r;
}

__attribute__((noinline)) static long deep(long n) {
    volatile char pad[512];
    pad[0] = (char)n;
    long r = n <= 0 ? 0 : deep(n - 1);
    return r + pad[0];
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0); // unbuffered: the ordering of a forked child\'s output is part of the comparison
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_flags = SA_SIGINFO;
    sa.sa_sigaction = handler;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGFPE, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);
    long acc = 0;
    for (int i = 0; i < 256; i++) {
        volatile long ok = i;
        if (sigsetjmp(pad, 1) == 0) acc += faulting(&ok, i, i + 1, i + 2, i + 3);
        if (sigsetjmp(pad, 1) == 0)
            acc += faulting((volatile long *)(intptr_t)8, i, i, i, i);
        else
            acc += 1000;
    }
    printf("segv caught=%ld sig=%ld code=%ld acc=%ld\n", caught, last_sig, last_code, acc);
    // integer divide by zero from a block the transliterator refuses (div/idiv)
    caught = 0;
    for (int i = 0; i < 32; i++) {
        volatile int z = 0, n = i + 1;
        if (sigsetjmp(pad, 1) == 0) { spin += n / z; }
    }
    printf("fpe caught=%ld sig=%ld\n", caught, last_sig);
    // guest stack overflow: the transliterator runs on the GUEST stack, so the
    // host SIGSEGV frame has nowhere to go without SA_ONSTACK.
    static char altstack[256 * 1024];
    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
    sigaltstack(&ss, NULL);
    memset(&sa, 0, sizeof sa);
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sa.sa_sigaction = handler;
    sigaction(SIGSEGV, &sa, NULL);
    caught = 0;
    if (sigsetjmp(pad, 1) == 0) { spin += deep(100000000); }
    printf("stack caught=%ld sig=%ld\n", caught, last_sig);
    return 0;
}
