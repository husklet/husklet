// ptrace_attach_blocked.c -- DEMONSTRATING repro (disposition excluded-known-bug) for a staged ptrace gap:
// PTRACE_ATTACH to a tracee that is ALREADY BLOCKED inside a syscall (here pause()). On native Linux the
// ATTACH sends the tracee a SIGSTOP that interrupts the blocking syscall, so the tracer's waitpid() returns
// the group-stop (WIFSTOPPED, WSTOPSIG==SIGSTOP) immediately -- this is how strace/gdb attach to an idle
// process. hl fires the attach-stop only at the tracee's NEXT guest syscall boundary; a tracee parked in a
// host-blocking syscall (pause/read/nanosleep) is never interrupted, so the tracer's waitpid does not
// observe the stop. Interrupting a blocked host syscall on ATTACH requires the async cross-process
// signal-delivery machinery (signal.c host_sigh_si / maybe_deliver_signal) to be wired into the ptrace
// attach path -- the same "async/cross-process signal-delivery stops are staged" limitation documented in
// os/linux/syscall/ptrace.c. The engine does NOT crash or wedge (it stays responsive; only this stop is
// missing) -- this test's own alarm(4) bounds it so it can never hang a runner.
//
// Native golden (correct behavior): ptrace-attach-blocked stop=1 sig=19
// Flip to disposition=active once ATTACH interrupts a syscall-blocked tracee.

#include <stdio.h>
#include <unistd.h>
#include <signal.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <time.h>

static volatile sig_atomic_t timed_out = 0;
static void on_alrm(int s) {
    (void)s;
    timed_out = 1;
}

int main(void) {
    pid_t c = fork();
    if (c < 0) { printf("ptrace-attach-blocked fork-fail\n"); return 1; }
    if (c == 0) {
        pause(); // park inside a host-blocking syscall before the parent attaches
        _exit(0);
    }
    struct timespec ts = {0, 200000000};
    nanosleep(&ts, NULL); // let the child reach pause()

    struct sigaction sa = {0};
    sa.sa_handler = on_alrm; // no SA_RESTART -> the alarm interrupts our blocking waitpid
    sigaction(SIGALRM, &sa, NULL);

    int stop = 0, sig = -1;
    if (ptrace(PTRACE_ATTACH, c, 0, 0) == 0) {
        alarm(4);
        int st = 0;
        pid_t r = waitpid(c, &st, 0);
        alarm(0);
        if (!timed_out && r == c && WIFSTOPPED(st)) {
            stop = 1;
            sig = WSTOPSIG(st);
        }
    }
    ptrace(PTRACE_KILL, c, 0, 0);
    kill(c, SIGKILL);
    waitpid(c, &(int){0}, 0);

    printf("ptrace-attach-blocked stop=%d sig=%d\n", stop, sig);
    return 0;
}
