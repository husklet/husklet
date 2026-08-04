// DISCOVERY probe: SA_SIGINFO SIGCHLD siginfo vs waitid for exit/kill/stop/cont fates.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile int g_code, g_status, g_pid, g_signo, g_n;
static void ch(int s, siginfo_t *si, void *u) {
    (void)s; (void)u;
    g_signo = si->si_signo; g_code = si->si_code; g_status = si->si_status; g_pid = si->si_pid; g_n++;
}

// SIGCHLD stays blocked outside this call, so exactly one delivery lands per step and the globals the
// caller then prints describe THAT step. `while (g_n == 0) pause();` could not promise either: the handler
// may run in the window between the g_n test and the pause() call, pause() then blocks until the NEXT
// SIGCHLD, and the print reads back the later event -- native reproduces the same misread verbatim once
// the parent is slowed between kill(SIGCONT) and the loop. Blocked-and-coalesced also pins the Linux rule
// the continued case is really about: a standard signal already pending keeps its FIRST siginfo, so a
// child continued and then exited in quick succession still reports CLD_CONTINUED.
static sigset_t g_chld_unblocked;
static void wait_chld(void) {
    while (g_n == 0) sigsuspend(&g_chld_unblocked);
}

int main(void) {
    struct sigaction sa; memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = ch; sa.sa_flags = SA_SIGINFO; // no SA_NOCLDSTOP: stop/cont generate SIGCHLD
    sigaction(SIGCHLD, &sa, NULL);
    sigset_t chld; sigemptyset(&chld); sigaddset(&chld, SIGCHLD);
    sigprocmask(SIG_BLOCK, &chld, &g_chld_unblocked);

    // exit
    g_n = 0;
    pid_t p = fork();
    if (p == 0) _exit(7);
    wait_chld();
    printf("exit signo=%d code=%d(want%d) status=%d pid=%d\n", g_signo, g_code, CLD_EXITED, g_status, g_pid == p);
    int st; waitpid(p, &st, 0);

    // killed
    g_n = 0;
    pid_t k = fork();
    if (k == 0) { pause(); _exit(0); }
    usleep(30000); kill(k, SIGKILL);
    wait_chld();
    printf("kill code=%d(want%d) status=%d(want%d)\n", g_code, CLD_KILLED, g_status, SIGKILL);
    waitpid(k, &st, 0);

    // stopped (SA_NOCLDSTOP absent => SIGCHLD on stop)
    g_n = 0;
    pid_t s = fork();
    if (s == 0) { raise(SIGSTOP); _exit(3); }
    wait_chld();
    printf("stop code=%d(want%d) status=%d(want%d)\n", g_code, CLD_STOPPED, g_status, SIGSTOP);
    // continued
    g_n = 0;
    kill(s, SIGCONT);
    wait_chld();
    printf("cont code=%d(want%d) status=%d(want%d)\n", g_code, CLD_CONTINUED, g_status, SIGCONT);
    waitpid(s, &st, 0);
    printf("stop-child-exit code=%d\n", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
    printf("done\n");
    return 0;
}
