// #392 stack-overflow guard regression test.
//
// A guest that runs off the BOTTOM of its stack must receive SIGSEGV at the guard page (exactly like
// Linux's stack guard gap) instead of silently corrupting hl's adjacent 64MB RX code cache. Before the
// fix the main stack sat immediately above the code cache with no guard, so a deep recursion wrote
// straight into the executable cache -> wild corruption (the clickhouse crash) rather than a clean fault.
//
// Verdict is a normalized, byte-identical line on every engine, diffed vs the native arm64 / qemu-x86_64
// oracle:
//   * deep_ok=1          -- a bounded-but-deep recursion still works (the guard lives BELOW the real
//                           limit, it does not eat into usable stack).
//   * overflow_sigsegv=1 -- a child that recurses without bound dies of SIGSEGV (reaped WIFSIGNALED,
//                           WTERMSIG==SIGSEGV), NOT a corrupted engine / wild crash / hang.
// The overflow runs in a forked CHILD so the parent can reap it and assert the exact termination signal
// (the same pattern LTP's pause02 / ext_sig/pausewake uses); this makes the verdict portable and exact.
#define _GNU_SOURCE
#include <stdio.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

// Bounded-but-deep recursion (~1MB): returns the depth so we can confirm it truly recursed with real
// frames. optimize("O0") keeps a real 512-byte frame per call so -O2 can't collapse it into a loop.
static long __attribute__((noinline, optimize("O0"))) deep(long n) {
    volatile char pad[512];
    pad[0] = (char)n;
    pad[511] = (char)~n;
    if (n == 0) return 0;
    return 1 + (pad[0] & 0) + (pad[511] & 0) + deep(n - 1);
}

// Unbounded recursion with a small real frame: walks the stack DOWN page-by-page and must strike the guard
// gap. optimize("O0") + the post-call use of pad defeat tail-call / recursion-to-loop optimization so the
// stack genuinely grows without bound (a plain -O2 build turns this into a stackless infinite loop).
static long __attribute__((noinline, optimize("O0"))) boom(long n) {
    volatile char pad[512];
    pad[0] = (char)n;
    long r = boom(n + 1);
    return r + pad[0];
}

int main(void) {
    int deep_ok = (deep(2000) == 2000); // usable stack works right up to a healthy depth

    pid_t pid = fork();
    if (pid == 0) {
        volatile long r = boom(1); // overrun the stack -> must hit the guard -> SIGSEGV
        (void)r;
        _exit(0); // unreachable
    }
    int st = 0;
    waitpid(pid, &st, 0);
    int overflow_sigsegv = WIFSIGNALED(st) && WTERMSIG(st) == SIGSEGV;

    printf("deep_ok=%d overflow_sigsegv=%d\n", deep_ok, overflow_sigsegv);
    return 0;
}
