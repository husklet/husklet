// getpid(2) stability + fork distinctness -- regression guard for the engine's cached host-pid fast path
// (container_pid). Two properties the cache must preserve: (1) getpid() returns ONE consistent value across
// a long burst of calls (the cache never drifts), and (2) the cache is dropped in the fork child, so the
// child's very FIRST post-fork getpid() already reports the child's OWN identity -- distinct from the parent
// and self-consistent -- rather than a stale inherited value; the parent's identity is intact afterwards.
// Uses only getpid(), so it makes no assumption about container pid virtualization and native + engine agree.
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

// getpid() positive and identical across a burst (via both the libc wrapper and the raw syscall).
static int pid_stable(pid_t expect) {
    if (expect <= 0) return 0;
    for (int i = 0; i < 300; i++) {
        if (getpid() != expect) return 0;
        if ((pid_t)syscall(SYS_getpid) != expect) return 0;
    }
    return 1;
}

int main(void) {
    pid_t parent = getpid();
    int parent_ok = pid_stable(parent);
    pid_t c = fork();
    if (c == 0) {
        // First getpid() in the child must already be the child's own new pid, not the inherited parent's.
        pid_t me = getpid();
        _exit((me != parent && pid_stable(me)) ? 0 : 1);
    }
    if (c < 0) {
        printf("getpid_fork_pid_cache parent=%d child=%d after=%d\n", parent_ok, 0, 0);
        return 1;
    }
    int st = 0;
    int child_ok = (waitpid(c, &st, 0) == c) && WIFEXITED(st) && WEXITSTATUS(st) == 0;
    int after_ok = pid_stable(parent); // parent's cached identity undisturbed by the child
    printf("getpid_fork_pid_cache parent=%d child=%d after=%d\n", parent_ok, child_ok, after_ok);
    return parent_ok && child_ok && after_ok ? 0 : 1;
}
