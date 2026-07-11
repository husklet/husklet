// Cross-mapping futex on a MAP_SHARED page reached at TWO DIFFERENT virtual addresses (the Chrome
// renderer<->GPU-service command-buffer pattern: one shared-memory object, mmap'd independently in each
// peer, so the SAME futex word lives at a DIFFERENT address in each). On Linux a FUTEX_WAIT and a
// FUTEX_WAKE on a MAP_SHARED page rendezvous by the shared key (inode+offset), NOT by virtual address,
// so a wake through mapping B must release a waiter parked through mapping A. dd hashed the futex bucket
// on the host VA, so the two mappings fell in different buckets and the wake was LOST (Wall 7).
//
//   T (same-process, two mappings of one memfd): thread FUTEX_WAITs via map A; main stores + FUTEX_WAKEs
//     via map B. Different VAs, same physical word -> the wake must cross.
//   X (cross-process): child mmaps the shared fd (its own VA) and FUTEX_WAITs; parent stores + FUTEX_WAKEs
//     via the parent's independent VA. Exactly the renderer/GPU-service split.
//
// Waits are TIMED (3s) so a LOST wake surfaces as woke=0 (a deterministic failure), never a harness hang.
// Linux-only raw futex(2) -> oracle-diffed (native prints woke=1 for both).
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long fwait(int *addr, int expected, const struct timespec *to) {
    return syscall(SYS_futex, addr, FUTEX_WAIT, expected, to, NULL, 0);
}
static long fwake(int *addr, int n) { return syscall(SYS_futex, addr, FUTEX_WAKE, n, NULL, NULL, 0); }

// A fresh anonymous shared-memory object of one page.
static int shared_page_fd(void) {
    int fd = (int)syscall(SYS_memfd_create, "futexkey", 0u);
    if (fd < 0) return -1;
    if (ftruncate(fd, 4096) != 0) return -1;
    return fd;
}

struct targs {
    int *word; // the WAITER's mapping of the shared word
    int woke;
};

// Park on the shared word and report whether a genuine cross-mapping WAKE arrived. woke=1 ONLY when a
// FUTEX_WAIT returns 0 (a delivered wake); a 2s ETIMEDOUT means the wake was LOST -> woke=0 even though
// the word is set (the store crossed via shared memory, but the WAKE never reached this bucket). Reading
// the word after a timeout would falsely score a lost wake as success, so we key strictly off fwait's rc.
static void *waiter_thread(void *p) {
    struct targs *a = p;
    struct timespec to = {2, 0};
    for (;;) {
        if (atomic_load((_Atomic int *)a->word) != 0) { // store beat us to the park -> not the lost-wake case
            a->woke = 1;
            return NULL;
        }
        long rc = fwait(a->word, 0, &to);
        if (rc == 0) {          // delivered wake (real or spurious): the WAKE reached this address's bucket
            a->woke = 1;
            return NULL;
        }
        if (rc == -1 && errno == ETIMEDOUT) { // no wake in 2s -> lost across the VA boundary
            a->woke = 0;
            return NULL;
        }
        // EAGAIN (word changed before parking) or EINTR: loop and re-evaluate.
    }
}

// ---- T: two mappings of one shared page in ONE process, wake must cross the VA boundary ----
static int test_two_mappings(void) {
    int fd = shared_page_fd();
    if (fd < 0) return -1;
    int *A = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int *B = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (A == MAP_FAILED || B == MAP_FAILED || A == B) return -1;
    A[0] = 0;
    struct targs a = {.word = A, .woke = 0};
    pthread_t th;
    if (pthread_create(&th, NULL, waiter_thread, &a) != 0) return -1;
    struct timespec nap = {0, 150 * 1000 * 1000};
    nanosleep(&nap, NULL);                  // let the waiter park in FUTEX_WAIT on mapping A
    atomic_store((_Atomic int *)&B[0], 1);  // flip the word through mapping B (same physical page)
    fwake(&B[0], INT_MAX);                   // wake through mapping B's address -> must reach A's waiter
    pthread_join(th, NULL);
    munmap(A, 4096);
    munmap(B, 4096);
    close(fd);
    return a.woke;
}

// ---- X: cross-process, child waits on its own mapping, parent wakes through the parent's mapping ----
static int test_xproc(void) {
    int fd = shared_page_fd();
    if (fd < 0) return -1;
    // Parent maps first; child maps after fork -> independent VAs for the same physical page.
    int *P = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (P == MAP_FAILED) return -1;
    P[0] = 0;
    int pipefd[2];
    if (pipe(pipefd) != 0) return -1;
    pid_t pid = fork();
    if (pid < 0) return -1;
    if (pid == 0) {
        close(pipefd[0]);
        // A dummy mapping shifts the child's allocator so its shared mapping lands at a DIFFERENT VA.
        (void)mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        int *C = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (C == MAP_FAILED) _exit(3);
        char b = 'R';
        (void)!write(pipefd[1], &b, 1); // tell the parent we are about to park
        struct timespec to = {2, 0};
        int woke = 0;
        for (;;) {
            if (atomic_load((_Atomic int *)C) != 0) { woke = 1; break; } // store beat the park
            long rc = fwait(C, 0, &to);
            if (rc == 0) { woke = 1; break; }                            // delivered cross-process wake
            if (rc == -1 && errno == ETIMEDOUT) { woke = 0; break; }     // lost across the VA boundary
        }
        _exit(woke ? 0 : 1);
    }
    close(pipefd[1]);
    char rb = 0;
    (void)!read(pipefd[0], &rb, 1); // wait until the child has its mapping and is entering the wait
    struct timespec nap = {0, 200 * 1000 * 1000};
    nanosleep(&nap, NULL);           // let the child actually park in FUTEX_WAIT
    atomic_store((_Atomic int *)&P[0], 1);
    fwake(&P[0], INT_MAX);           // wake through the parent's independent VA
    int st = 0;
    waitpid(pid, &st, 0);
    munmap(P, 4096);
    close(fd);
    close(pipefd[0]);
    return (WIFEXITED(st) && WEXITSTATUS(st) == 0) ? 1 : 0;
}

int main(void) {
    int t = test_two_mappings();
    int x = test_xproc();
    printf("futex_shared_key two_map_woke=%d xproc_woke=%d\n", t, x);
    return 0;
}
