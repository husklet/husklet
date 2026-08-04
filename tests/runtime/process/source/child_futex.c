// Regression for the single-threaded-parent fork fast path (thread_after_fork skips the private-futex
// table reset + registry rebuild when the forking process had no peer thread). Proves the child can still
// spawn threads and drive a PROCESS-PRIVATE futex (FUTEX_WAIT/FUTEX_WAKE), use an eventfd, and that a
// NESTED fork inside the child works -- i.e. the inherited-but-not-reset private futex table is correct.
// A stale/held bucket lock or lost waiter slot would hang or drop the wake; deterministic golden catches it.
#define _GNU_SOURCE
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static atomic_int word = 0;
static volatile int woke = 0;

static long fwait(int *a, int e) { return syscall(SYS_futex, a, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, e, NULL, NULL, 0); }
static long fwake(int *a, int n) { return syscall(SYS_futex, a, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, n, NULL, NULL, 0); }

static void *waiter(void *arg) {
    (void)arg;
    while (atomic_load(&word) == 0) fwait((int *)&word, 0);
    woke = 1;
    return NULL;
}

// Everything the child does after fork: drive a private futex between two NEW threads it spawns, an
// eventfd round trip, and a nested grandchild. Returns a deterministic code folded into the parent sum.
static int child_body(void) {
    int ok = 0;
    pthread_t t;
    pthread_create(&t, NULL, waiter, NULL);            // a peer thread first appears only HERE, in the child
    struct timespec s = { .tv_sec = 0, .tv_nsec = 20000000 };
    nanosleep(&s, NULL);                               // let it park in the private-futex bucket
    atomic_store(&word, 1);
    long n = fwake((int *)&word, 1);
    pthread_join(t, NULL);
    ok += (woke == 1);                                 // the parked waiter was woken through the private table
    ok += (n >= 0);

    int efd = eventfd(0, 0);
    uint64_t v = 7;
    ok += (write(efd, &v, sizeof v) == (ssize_t)sizeof v);
    v = 0;
    ok += (read(efd, &v, sizeof v) == (ssize_t)sizeof v && v == 7);
    close(efd);

    pid_t g = fork();                                  // nested fork from the (now multi-threaded-history) child
    if (g == 0) _exit(3);
    int gst = 0;
    if (waitpid(g, &gst, 0) == g && WIFEXITED(gst)) ok += (WEXITSTATUS(gst) == 3);
    return ok; // 5 checks -> deterministic 5 per round
}

int main(void) {
    long sum = 0;
    for (int i = 0; i < 4; i++) {                      // main stays single-threaded across every fork
        pid_t p = fork();
        if (p == 0) _exit(child_body());
        int st = 0;
        if (waitpid(p, &st, 0) != p || !WIFEXITED(st)) { printf("fork_child_futex reap_fail@%d\n", i); return 1; }
        sum += WEXITSTATUS(st);
    }
    printf("fork_child_futex rounds=4 sum=%ld\n", sum);
    return 0;
}
