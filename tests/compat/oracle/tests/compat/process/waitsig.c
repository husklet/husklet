// #403 guard — a guest child killed by a fatal-default signal must be reported to the parent's wait4 as
// WIFSIGNALED/WTERMSIG=signo (not WIFEXITED(128+signo)), including the signals with NO faithful fatal host
// (macOS) mapping: SIGPOLL(29)/SIGSTKFLT(16) map to host signals that default-IGNORE, SIGPWR(30) maps to a
// host signal that reports a different signo. hl relays the intended Linux signo through a shared table and
// the parent reconstructs the status. This is the LTP waitpid01 scenario, distilled to a single golden line
// diffable against the native oracle on both Linux engines.
//
// Coverage:
//   - SIGPOLL/SIGSYS/SIGSTKFLT/SIGPWR: die via raise(sig) -> the #401/#403 relay path.
//   - exit157: a REAL _exit(157) must read back WIFEXITED(157), never a signal death (disambiguation guard).
//   - sigkill: parent kill(child, SIGKILL) -> the cross-process host-kill path (must not regress).
//   - sigsegv: an actual NULL write -> the synchronous-fault path (must not regress).
// RLIMIT_CORE is pinned to 0 so WCOREDUMP is deterministically 0 on every case (hl == native), independent
// of the host's core policy. (The WCOREDUMP==1 path is exercised by LTP waitpid01's coredump variant.)

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

// Fork a child that dies by raising `sig` with the default disposition; verify the parent observes
// WIFSIGNALED, WTERMSIG==sig, WCOREDUMP==0. Returns 1 on full match.
static int check_raise(int sig) {
    pid_t p = fork();
    if (p == 0) {
        signal(sig, SIG_DFL);
        raise(sig);
        _exit(199); // reached only if the signal failed to terminate us -> parent flags a failure
    }
    int st = 0;
    if (waitpid(p, &st, 0) != p) return 0;
    return WIFSIGNALED(st) && WTERMSIG(st) == sig && WCOREDUMP(st) == 0;
}

// A real _exit(code): the parent must read WIFEXITED(code), NOT mistake it for a signal death.
static int check_exit(int code) {
    pid_t p = fork();
    if (p == 0) _exit(code);
    int st = 0;
    if (waitpid(p, &st, 0) != p) return 0;
    return WIFEXITED(st) && WEXITSTATUS(st) == code && !WIFSIGNALED(st);
}

// Parent kills the child with SIGKILL (cross-process host-kill path).
static int check_kill(void) {
    pid_t p = fork();
    if (p == 0) {
        for (;;)
            pause();
        _exit(199);
    }
    kill(p, SIGKILL);
    int st = 0;
    if (waitpid(p, &st, 0) != p) return 0;
    return WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL && WCOREDUMP(st) == 0;
}

// Child takes a real SIGSEGV (NULL write) -> synchronous-fault death path.
static int check_segv(void) {
    pid_t p = fork();
    if (p == 0) {
        signal(SIGSEGV, SIG_DFL);
        *(volatile int *)0 = 1;
        _exit(199);
    }
    int st = 0;
    if (waitpid(p, &st, 0) != p) return 0;
    return WIFSIGNALED(st) && WTERMSIG(st) == SIGSEGV && WCOREDUMP(st) == 0;
}

int main(void) {
    struct rlimit rl = {0, 0}; // disable core dumps -> WCOREDUMP deterministically 0 everywhere
    setrlimit(RLIMIT_CORE, &rl);

    int sigpoll = check_raise(SIGPOLL);       // 29 -> host SIGIO (default ignore): the headline #403 bug
    int sigsys = check_raise(SIGSYS);         // 31
    int sigstkflt = check_raise(SIGSTKFLT);   // 16 -> host SIGURG (default ignore)
    int sigpwr = check_raise(SIGPWR);         // 30 -> host SIGUSR1 (terminates, but reports a different signo)
    int exit157 = check_exit(157);            // disambiguation: a real exit(128+29) must stay WIFEXITED
    int sigkill = check_kill();               // 9  (host-kill path, must not regress)
    int sigsegv = check_segv();               // 11 (fault path, must not regress)

    printf("waitsig sigpoll=%d sigsys=%d sigstkflt=%d sigpwr=%d exit157=%d sigkill=%d sigsegv=%d\n", sigpoll,
           sigsys, sigstkflt, sigpwr, exit157, sigkill, sigsegv);
    return 0;
}
