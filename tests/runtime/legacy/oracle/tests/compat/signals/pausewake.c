// pause() WAKE semantics (task #397): LTP pause01 + pause02. A signal that is CAUGHT (a guest handler is
// installed) must wake pause() -> it returns -1/EINTR after the handler runs; this must hold for EVERY
// deliverable signal delivered via kill(2), INCLUDING the fault-class ones (SIGILL/SIGTRAP/SIGFPE/SIGSEGV/
// SIGBUS) which hl otherwise routed to its hardware-fault guard and never woke pause (the pause01 BROK).
// pause02: a SIGKILL is un-catchable -> pause never returns and the process dies by SIGKILL. Delivery is
// via alarm()/SIGALRM and a child->parent (or parent->child) kill; every line is a normalized verdict, so
// the output is byte-identical on both guest arches and is diffed against the native oracle.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t got;
static void h(int s) { got = s; }

// A child installs a handler for `sig`, then pause()s; the parent kills it with `sig`. The child must
// resume from pause() with -1/EINTR and its handler must have run. Returns 1 on the correct outcome.
static int caught_wakes(int sig) {
    pid_t pid = fork();
    if (pid == 0) {
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_handler = h;
        sigfillset(&sa.sa_mask);
        sigaction(sig, &sa, NULL);
        got = 0;
        errno = 0;
        int rc = pause();
        _exit(rc == -1 && errno == EINTR && got == sig ? 0 : 1);
    }
    usleep(120000); // let the child reach pause()
    kill(pid, sig);
    int st;
    waitpid(pid, &st, 0);
    return WIFEXITED(st) && WEXITSTATUS(st) == 0;
}

// pause() woken by our own alarm()/SIGALRM in-process (self-directed timer signal).
static int alarm_wakes(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigaction(SIGALRM, &sa, NULL);
    got = 0;
    errno = 0;
    alarm(1);
    int rc = pause();
    return rc == -1 && errno == EINTR && got == SIGALRM;
}

// pause02: SIGKILL cannot be caught -> pause() never returns; the child dies BY SIGKILL.
static int sigkill_kills(void) {
    pid_t pid = fork();
    if (pid == 0) {
        pause();
        _exit(7); // must never be reached
    }
    usleep(120000);
    kill(pid, SIGKILL);
    int st;
    waitpid(pid, &st, 0);
    return WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL;
}

int main(void) {
    // The full LTP pause01 signal set (each caught, delivered by kill(2)).
    int sigs[] = {SIGHUP, SIGINT,  SIGQUIT, SIGILL,  SIGTRAP, SIGABRT,
                  SIGFPE, SIGSEGV, SIGBUS,  SIGPIPE, SIGALRM, SIGTERM, SIGUSR1, SIGUSR2};
    const char *nm[] = {"HUP", "INT",  "QUIT", "ILL",  "TRAP", "ABRT", "FPE",
                        "SEGV", "BUS", "PIPE", "ALRM", "TERM", "USR1", "USR2"};
    int all = 1;
    for (unsigned i = 0; i < sizeof sigs / sizeof sigs[0]; i++) {
        int ok = caught_wakes(sigs[i]);
        all &= ok;
        printf("wake_%s=%d\n", nm[i], ok);
    }
    printf("wake_alarm=%d\n", alarm_wakes());
    printf("sigkill_kills=%d\n", sigkill_kills());
    printf("all_caught_wake=%d\n", all);
    return 0;
}
