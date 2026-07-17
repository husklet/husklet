// Minimized differential guard for the x86_64 non-PIE (static -no-pie ET_EXEC) thread-creation bug:
// EVERY spawned pthread in such a binary SIGSEGV'd on the x86 engine (glibc's per-thread `tcache` is
// initialized from the .tdata TLS template to a baked-LOW pointer &__tcache_dummy; the engine biased
// address-materialization (`lea sym(%rip)`) HIGH, so glibc's thread-exit sentinel comparison of the two
// diverged and it freed the sentinel). A bare thread that touches NEITHER TLS nor the canary already
// crashed, so this fixture deliberately ALSO reads a __thread variable (thread-local storage) and a
// stack-canary-protected frame (the char buffer forces -fstack-protector's %fs:0x28 read) inside the
// spawned thread, so both the plain thread-setup path and the %fs TLS/canary path are oracle-checked.
// Built static -no-pie (src_nopie) and diffed byte-exact vs the qemu-x86_64 oracle on the JIT.
#include <pthread.h>
#include <stdio.h>
#include <string.h>

#define N 8

static __thread long tls_slot;      // local-exec TLS: read+written from each spawned thread

// Char buffer + memset => -fstack-protector installs a canary (mov %fs:0x28,...); reading it in the
// spawned thread exercises the %fs TLS-base path that a wrong child fs_base would corrupt.
__attribute__((noinline)) static long canary_frame(long seed) {
    char buf[64];
    memset(buf, (int)(seed & 0x7f), sizeof buf);
    long acc = 0;
    for (unsigned i = 0; i < sizeof buf; i++) acc += (unsigned char)buf[i];
    return acc;
}

static void *worker(void *arg) {
    long id = (long)arg;
    tls_slot = id * 100;             // write this thread's local storage
    long c = canary_frame(id + 1);   // stack-canary-protected frame
    return (void *)(tls_slot + c);   // per-thread local value survives + canary frame returned
}

int main(void) {
    pthread_t t[N];
    long total = 0;
    for (long i = 0; i < N; i++) pthread_create(&t[i], 0, worker, (void *)i);
    for (long i = 0; i < N; i++) {
        void *r = 0;
        pthread_join(t[i], &r);
        total += (long)r;
    }
    printf("nopie-thread tls+canary total=%ld\n", total); // deterministic
    return 0;
}
