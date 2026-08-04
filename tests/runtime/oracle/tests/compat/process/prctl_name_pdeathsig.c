// prctl coverage:
//   - PR_SET_NAME / PR_GET_NAME round-trips the (<=15 char) process name.
//   - PR_SET_PDEATHSIG get/set round-trips.
//   - Live parent-death signal: a grandchild arms PR_SET_PDEATHSIG(SIGUSR1); when its immediate parent
//     dies the kernel delivers that signal. The grandchild sigwait()s for it and reports success up a
//     pipe. The top process is a child-subreaper so the orphaned grandchild is still reapable.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    // 1. name round-trip
    char name[16] = {0};
    int set_name = prctl(PR_SET_NAME, (unsigned long)"hlprobe", 0, 0, 0) == 0;
    prctl(PR_GET_NAME, (unsigned long)name, 0, 0, 0);
    int name_ok = set_name && strcmp(name, "hlprobe") == 0;

    // 2. pdeathsig get/set round-trip
    int getsig = -1;
    int set_pd = prctl(PR_SET_PDEATHSIG, SIGUSR2, 0, 0, 0) == 0;
    prctl(PR_GET_PDEATHSIG, (unsigned long)&getsig, 0, 0, 0);
    int pd_roundtrip = set_pd && getsig == SIGUSR2;
    prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0); // clear so it doesn't affect us

    // 3. live delivery
    prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    int pfd[2];
    if (pipe(pfd) != 0) { printf("pipe fail\n"); return 1; }

    pid_t mid = fork();
    if (mid == 0) {
        close(pfd[0]);
        pid_t gc = fork();
        if (gc == 0) {
            // grandchild: block SIGUSR1, arm pdeathsig, then wait for parent death
            sigset_t set;
            sigemptyset(&set);
            sigaddset(&set, SIGUSR1);
            sigprocmask(SIG_BLOCK, &set, NULL);
            prctl(PR_SET_PDEATHSIG, SIGUSR1, 0, 0, 0);
            // guard: if our parent already died before arming, exit as failure via pipe
            if (getppid() == 1) { char b = 'x'; if (write(pfd[1], &b, 1)) {} _exit(0); }
            int sig = 0;
            sigwait(&set, &sig);
            char b = (sig == SIGUSR1) ? 'y' : 'n';
            if (write(pfd[1], &b, 1)) {}
            _exit(0);
        }
        usleep(50 * 1000); // let the grandchild arm pdeathsig
        _exit(0);          // die -> grandchild gets SIGUSR1
    }
    close(pfd[1]);
    char got = '?';
    if (read(pfd[0], &got, 1) != 1) got = '?';
    // reap everyone (subreaper harvests the reparented grandchild)
    int any;
    do { int s; any = waitpid(-1, &s, 0); } while (any > 0);

    int delivered = got == 'y';
    printf("prctl name_ok=%d pd_roundtrip=%d delivered=%d\n", name_ok, pd_roundtrip, delivered);
    return 0;
}
