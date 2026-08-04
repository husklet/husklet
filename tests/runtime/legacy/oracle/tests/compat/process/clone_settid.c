// raw clone(2) with CLONE_VM | CLONE_PARENT_SETTID | CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID on a
// distinct (waitable, SIGCHLD) child that shares the address space so the tid futex word is shared.
// Verifies:
//   - parent_tid was written by the kernel and equals the returned child pid,
//   - the child's own settid slot matches,
//   - CHILD_CLEARTID: the kernel zeroes the ctid word and does a FUTEX_WAKE on it at child exit,
//     which is exactly how pthread_join / thread teardown observes death.
// All output is derived booleans -- no raw tids.
#define _GNU_SOURCE
#include <linux/futex.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static pid_t g_ptid, g_ctid;

static long fwait(volatile int *addr, int expected) {
    return syscall(SYS_futex, addr, FUTEX_WAIT, expected, NULL, NULL, 0);
}

static int child_fn(void *arg) {
    // child writes what it sees for its own settid slot vs actual tid
    volatile int *ctid = (volatile int *)arg;
    // give the parent a moment to observe the pre-exit (nonzero) ctid
    usleep(30 * 1000);
    // its own tid should match what CHILD_SETTID wrote (we can only see via gettid here)
    pid_t tid = (pid_t)syscall(SYS_gettid);
    _exit(*ctid == tid ? 0 : 1);
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    size_t stack_size = 64 * 1024;
    void *stack = mmap(NULL, stack_size, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (stack == MAP_FAILED) { printf("mmap fail\n"); return 1; }
    (void)page;

    g_ptid = -1;
    g_ctid = -1;
    void *stack_top = (char *)stack + stack_size;

    int flags = CLONE_VM | CLONE_PARENT_SETTID | CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID | SIGCHLD;
    pid_t pid = clone(child_fn, stack_top, flags, (void *)&g_ctid, &g_ptid, NULL, &g_ctid);
    if (pid < 0) { printf("clone fail\n"); return 1; }

    int ptid_ok = g_ptid == pid;                 // PARENT_SETTID wrote the child tid
    // CHILD_CLEARTID: wait on the ctid futex; when the child exits the kernel zeroes it and wakes us.
    while (g_ctid != 0) {
        fwait((volatile int *)&g_ctid, g_ctid);   // returns on wake or EAGAIN if already changed
    }
    int cleared = g_ctid == 0;                    // ctid zeroed on child exit

    int st = 0;
    waitpid(pid, &st, 0);
    int child_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;

    printf("clone_settid ptid_ok=%d cleared=%d child_ok=%d\n", ptid_ok, cleared, child_ok);
    munmap(stack, stack_size);
    return 0;
}
