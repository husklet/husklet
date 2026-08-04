// gettid/getpid identity across fork generations. Regression guard for the per-thread task-state slot
// cache in the engine: a forked child must re-derive its OWN pid (the cache the child inherits from the
// parent is dropped on fork), and every post-fork syscall must observe the child's identity -- not a stale
// parent slot. Exercises: (1) tid==pid in a single-threaded process, (2) a child's pid differs from the
// parent and its tid tracks its new pid, (3) the same again one generation deeper (grandchild), and
// (4) a burst of syscalls in each child stays self-consistent. Pure POSIX semantics, so native and the
// engine agree exactly.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

// gettid==getpid held stably across a burst of syscalls, and both positive.
static int self_consistent(void) {
    pid_t pid = getpid();
    if (pid <= 0) return 0;
    for (int i = 0; i < 200; i++) {
        pid_t tid = (pid_t)syscall(SYS_gettid);
        if (tid != pid) return 0;
        if ((pid_t)getpid() != pid) return 0;
    }
    return 1;
}

// Run body in a child; return 1 iff the child exited 0.
static int in_child(int (*body)(pid_t parent), pid_t parent) {
    pid_t c = fork();
    if (c == 0) _exit(body(parent) ? 0 : 1);
    if (c < 0) return 0;
    int st = 0;
    if (waitpid(c, &st, 0) != c) return 0;
    return WIFEXITED(st) && WEXITSTATUS(st) == 0;
}

static int grandchild_body(pid_t parent) {
    // Deeper generation: distinct pid from BOTH ancestors and self-consistent.
    pid_t me = getpid();
    return me != parent && self_consistent();
}

static int child_body(pid_t parent) {
    pid_t me = getpid();
    if (me == parent || !self_consistent()) return 0;
    // Fork once more from inside the child so the grandchild re-derives yet again.
    return in_child(grandchild_body, me);
}

int main(void) {
    pid_t self = getpid();
    int parent_ok = self_consistent();
    int child_ok = in_child(child_body, self);
    int after_ok = self_consistent(); // parent's own slot still correct after the children ran
    printf("gettid_fork_ident parent=%d child=%d after=%d\n", parent_ok, child_ok, after_ok);
    return parent_ok && child_ok && after_ok ? 0 : 1;
}
