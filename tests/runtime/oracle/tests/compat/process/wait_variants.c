// wait() / waitpid() / wait4()+rusage / waitid(WNOWAIT then reap) / final ECHILD. All derived booleans
// and small counts; rusage is only checked for structural sanity (non-negative maxrss), never exact.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    // 1. bare wait()
    pid_t a = fork();
    if (a == 0) _exit(3);
    int sa = 0;
    int wait_ok = wait(&sa) == a && WIFEXITED(sa) && WEXITSTATUS(sa) == 3;

    // 2. waitpid targeted
    pid_t b = fork();
    if (b == 0) _exit(4);
    int sb = 0;
    int waitpid_ok = waitpid(b, &sb, 0) == b && WEXITSTATUS(sb) == 4;

    // 3. wait4 with rusage
    pid_t c = fork();
    if (c == 0) _exit(5);
    int sc = 0;
    struct rusage ru;
    int wait4_ok = wait4(c, &sc, 0, &ru) == c && WEXITSTATUS(sc) == 5 && ru.ru_maxrss >= 0;

    // 4. waitid WNOWAIT leaves the child in a reapable state, then a real reap succeeds
    pid_t d = fork();
    if (d == 0) _exit(6);
    siginfo_t si; si.si_pid = 0;
    int peek = waitid(P_PID, d, &si, WEXITED | WNOWAIT) == 0 && si.si_status == 6 && si.si_code == CLD_EXITED;
    int sd = 0;
    int real_reap = waitpid(d, &sd, 0) == d && WEXITSTATUS(sd) == 6; // still reapable after WNOWAIT

    // 5. no children left -> ECHILD
    errno = 0;
    int echild = wait(NULL) == -1 && errno == ECHILD;

    printf("wait=%d waitpid=%d wait4=%d wnowait=%d reap=%d echild=%d\n",
           wait_ok, waitpid_ok, wait4_ok, peek, real_reap, echild);
    return 0;
}
