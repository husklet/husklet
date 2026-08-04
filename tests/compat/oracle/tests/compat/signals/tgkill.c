// tgkill(2)/gettid: direct thread-directed signal to a specific tid within our tgid. Linux-only
// (raw syscalls) -> native oracle.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile sig_atomic_t got = 0;
static void h(int s) { (void)s; got = 1; }

int main(void) {
    signal(SIGUSR1, h);
    pid_t tgid = getpid();
    pid_t tid = (pid_t)syscall(SYS_gettid);
    int self_tid = tid == tgid;                  // single-threaded: tid == pid
    long rc = syscall(SYS_tgkill, tgid, tid, SIGUSR1);
    int delivered = rc == 0 && got == 1;
    printf("tgkill self_tid=%d rc=%ld delivered=%d\n", self_tid, rc, delivered);
    return 0;
}
