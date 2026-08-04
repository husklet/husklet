#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

// Concurrency regression for the private-anon PROT tracker (g_anonmap in helpers.c). Many threads at
// once mutate the tracker: each thread owns a DISJOINT address window and loops
//   mprotect(RO) -> read-verify -> mprotect(RW) -> MADV_DONTNEED -> read-back-zero -> write -> verify
// which drives anon_update_prot() (the split-prone writer), anon_prot_if_contained() (DONTNEED remap +
// the SIGSEGV representation-repair reader), and anon_track()/anon_untrack() (the mmap/munmap that
// bracket each window). With the registry lock-free, a reader could see a torn entry and a realloc could
// free the array under a peer reader (use-after-free); serializing every op fixes it. The coherence
// invariant checked here: because each window is thread-private, every read observes exactly what THIS
// thread last wrote/protected. If the tracker returned a stale/torn/missed prot, MADV_DONTNEED would
// re-establish the range at the wrong protection (or skip it), so the zero read-back below would instead
// see the old pattern -- or a write to a wrongly-PROT_NONE'd page would fault.
//
// Each window is mmap'd once and munmap'd once (concurrent anon_track/anon_untrack) while the hot loop
// keeps mprotect/madvise churning it; this concentrates the stress on the tracker itself rather than on
// unrelated host MAP_FIXED remap machinery. ENOMEM anywhere is tolerated (VMA pressure from splitting is
// a real Linux mprotect(2) verdict, not a crash or coherence break): the thread just retries the step.

enum { THREADS = 8, ITERS = 1200, LENGTH = 0x4000, STRIDE = 0x40000 };

static uintptr_t g_base;

static int transient(void) { return errno == ENOMEM; }

static void *worker(void *arg) {
    unsigned tid = (unsigned)(uintptr_t)arg;
    unsigned char *addr = (unsigned char *)(g_base + (uintptr_t)tid * STRIDE);
    unsigned char pat = (unsigned char)(0x40 + tid);

    // anon_track: establish this thread's private-anon window (concurrent with every peer's mmap).
    if (mmap(addr, LENGTH, PROT_READ | PROT_WRITE,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) != addr)
        return (void *)1;
    memset(addr, pat, LENGTH);

    for (unsigned it = 0; it < ITERS; ++it) {
        // anon_update_prot: flip the whole window read-only, then back. Concurrency here is what tore the
        // lock-free registry (split rewrite + head/tail append racing a peer's scan).
        if (mprotect(addr, LENGTH, PROT_READ) != 0) {
            if (transient()) continue;
            return (void *)2;
        }
        if (addr[0] != pat || addr[LENGTH - 1] != pat) return (void *)3; // read-only: value intact
        if (mprotect(addr, LENGTH, PROT_READ | PROT_WRITE) != 0) {
            if (transient()) continue;
            return (void *)4;
        }
        // anon_prot_if_contained (via the MADV_DONTNEED handler): must report THIS window's CURRENT (RW)
        // prot so the range is re-established writable + zero. A stale/missed verdict breaks the readback.
        if (madvise(addr, LENGTH, MADV_DONTNEED) != 0) {
            if (transient()) continue;
            return (void *)5;
        }
        if (addr[0] != 0 || addr[LENGTH - 1] != 0) return (void *)6; // DONTNEED must zero -> tracker coherent
        memset(addr, pat, LENGTH);                                   // still RW -> store must succeed
        if (addr[LENGTH / 2] != pat) return (void *)7;
    }

    // anon_untrack: retire the window (concurrent with every peer's munmap).
    if (munmap(addr, LENGTH) != 0) return (void *)8;
    return (void *)0;
}

static int spin(void) {
    pthread_t th[THREADS];
    for (unsigned i = 0; i < THREADS; ++i)
        if (pthread_create(&th[i], NULL, worker, (void *)(uintptr_t)i) != 0) return 0;
    int ok = 1;
    for (unsigned i = 0; i < THREADS; ++i) {
        void *r = (void *)1;
        pthread_join(th[i], &r);
        if (r != (void *)0) ok = 0;
    }
    return ok;
}

int main(void) {
    g_base = UINT64_C(0x600000000);
    int threaded = spin();

    // fork-under-threads variant: exercises anon_after_fork(). The parent's peer threads have joined, but
    // the child re-inits the inherited registry lock and must itself run the full concurrent churn without
    // wedging on an inherited-locked mutex.
    g_base = UINT64_C(0x680000000);
    pid_t child = fork();
    if (child == 0) _exit(spin() ? 0 : 1);
    int status = 0;
    int forked = (child > 0) && waitpid(child, &status, 0) == child &&
                 WIFEXITED(status) && WEXITSTATUS(status) == 0;

    printf("anon-tracker-concurrent threads=%d threaded=%d forked=%d\n", THREADS, threaded, forked);
    return threaded && forked ? 0 : 1;
}
