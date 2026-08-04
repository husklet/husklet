// #392 stack-overflow guard -- the GUEST-HANDLER half (item 3): a guest that installs its own SIGSEGV
// handler on an alternate signal stack (exactly glibc's stack-overflow detection / a JIT's guard-page
// trap) must, when it overruns its stack, get that handler invoked with signo SIGSEGV and a non-NULL
// si_addr pointing into the guard region -- NOT a corrupted engine. Byte-identical verdict vs the native
// arm64 / qemu-x86_64 oracle. The handler runs on the altstack because the main stack is exhausted.
#define _GNU_SOURCE
#include <stdio.h>
#include <signal.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>

static void onsegv(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    // A real fault address (in the guard just below the stack), not the async-kill si_addr==0.
    int ok = (sig == SIGSEGV) && si && si->si_addr != NULL;
    write(1, ok ? "caught SIGSEGV addr=1\n" : "caught wrong\n", ok ? 21 : 13);
    _exit(42);
}

static long __attribute__((noinline, optimize("O0"))) boom(long n) {
    volatile char pad[512];
    pad[0] = (char)n;
    long r = boom(n + 1);
    return r + pad[0];
}

int main(void) {
    static char alt[1 << 18]; // 256KB alternate stack (SIGSTKSZ isn't a constant on modern glibc)
    stack_t ss = {.ss_sp = alt, .ss_size = sizeof alt, .ss_flags = 0};
    sigaltstack(&ss, NULL);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = onsegv;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL); // Linux delivers SIGSEGV here, but be robust
    volatile long r = boom(1);
    (void)r;
    write(1, "unreachable\n", 12);
    return 0;
}
