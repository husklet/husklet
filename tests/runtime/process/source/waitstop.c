// Job-control stop/continue wait-status encoding. A child stopped by SIGSTOP/SIGTSTP is reaped with
// WUNTRACED (WIFSTOPPED + WSTOPSIG), continued with SIGCONT and reaped with WCONTINUED (WIFCONTINUED,
// raw status 0xffff), and -- the regression this pins -- must then RESUME and run to its real exit so the
// parent's next wait() reports the child's true WEXITSTATUS. hl historically forced the guest to terminate
// with 128+stopsig (e.g. status 0x9300 / WEXITSTATUS 147 for SIGSTOP) the instant it was continued, losing
// the real exit code and breaking every shell/supervisor that stops-then-continues a job. Also cross-checks
// the waitid(2) siginfo (CLD_STOPPED/CLD_CONTINUED) for the same fates and WNOWAIT re-reapability.
// Deterministic (booleans + fixed codes only) -> golden + oracle-diffed against native.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    // 1. SIGSTOP: WUNTRACED stop report, SIGCONT + WCONTINUED, then the child's real exit(4) survives.
    {
        pid_t p = fork();
        if (p == 0) { raise(SIGSTOP); _exit(4); }
        int st = 0;
        pid_t r = waitpid(p, &st, WUNTRACED);
        printf("sigstop stopped=%d stopsig=%d raw=0x%x r=%d\n",
               WIFSTOPPED(st) ? 1 : 0, WIFSTOPPED(st) ? WSTOPSIG(st) : -1, st & 0xffff, r == p);
        kill(p, SIGCONT);
        int sc = 0;
        pid_t rc = waitpid(p, &sc, WCONTINUED);
        printf("sigcont continued=%d raw=0x%x r=%d\n", WIFCONTINUED(sc) ? 1 : 0, sc & 0xffff, rc == p);
        int se = 0;
        waitpid(p, &se, 0);
        printf("resumed-exit exited=%d code=%d raw=0x%x\n",
               WIFEXITED(se) ? 1 : 0, WIFEXITED(se) ? WEXITSTATUS(se) : -1, se & 0xffff);
    }

    // 2. SIGTSTP + WUNTRACED, continue, then exit(9) survives (a second stop signal, exercised via waitid).
    {
        pid_t p = fork();
        if (p == 0) { raise(SIGTSTP); _exit(9); }
        siginfo_t si;
        memset(&si, 0, sizeof si);
        // WNOWAIT peek of the stop, then a real WSTOPPED reap -- both must report CLD_STOPPED.
        waitid(P_PID, p, &si, WSTOPPED | WNOWAIT);
        int peek = (si.si_signo == SIGCHLD && si.si_code == CLD_STOPPED && si.si_status == SIGTSTP && si.si_pid == p);
        memset(&si, 0, sizeof si);
        waitid(P_PID, p, &si, WSTOPPED);
        int stop = (si.si_code == CLD_STOPPED && si.si_status == SIGTSTP);
        printf("tstp peek=%d stop=%d\n", peek, stop);
        kill(p, SIGCONT);
        memset(&si, 0, sizeof si);
        waitid(P_PID, p, &si, WCONTINUED);
        printf("tstp cont code=%d status=%d\n", si.si_code == CLD_CONTINUED, si.si_status == SIGCONT);
        int se = 0;
        waitpid(p, &se, 0);
        printf("tstp resumed code=%d\n", WIFEXITED(se) ? WEXITSTATUS(se) : -1);
    }

    // 3. waitid CLD_EXITED / CLD_KILLED fates + WNOWAIT leaves the zombie reapable a second time.
    {
        pid_t p = fork();
        if (p == 0) _exit(9);
        siginfo_t si;
        memset(&si, 0, sizeof si);
        waitid(P_PID, p, &si, WEXITED | WNOWAIT);
        int peeked = (si.si_code == CLD_EXITED && si.si_status == 9 && si.si_pid == p);
        memset(&si, 0, sizeof si);
        int r2 = waitid(P_PID, p, &si, WEXITED);
        printf("wnowait peeked=%d reaped_again=%d\n",
               peeked, (r2 == 0 && si.si_code == CLD_EXITED && si.si_status == 9));

        pid_t k = fork();
        if (k == 0) { pause(); _exit(0); }
        usleep(40000);
        kill(k, SIGKILL);
        memset(&si, 0, sizeof si);
        waitid(P_PID, k, &si, WEXITED);
        printf("killed code=%d status=%d\n", si.si_code == CLD_KILLED, si.si_status == SIGKILL);
    }

    // 4. waitpid pid selection: 0 => any child in caller's pgrp, -pgid => that group; WNOHANG=0 when unready;
    //    ECHILD when no children; double-wait on a reaped pid => ECHILD.
    {
        pid_t p = fork();
        if (p == 0) _exit(11);
        int st = 0;
        pid_t r = waitpid(0, &st, 0);
        printf("pgrp0 ok=%d code=%d\n", r == p, WEXITSTATUS(st));

        pid_t q = fork();
        if (q == 0) _exit(13);
        int st2 = 0;
        pid_t r2 = waitpid(-getpgrp(), &st2, 0);
        printf("pgrpn ok=%d code=%d\n", r2 == q, WEXITSTATUS(st2));

        pid_t w = fork();
        if (w == 0) { usleep(60000); _exit(1); }
        int st3 = 0;
        pid_t rn = waitpid(w, &st3, WNOHANG);
        printf("wnohang_notready ret=%d\n", (int)rn);
        waitpid(w, &st3, 0);
        errno = 0;
        pid_t rc = waitpid(w, &st3, 0);
        printf("double echild=%d\n", rc == -1 && errno == ECHILD);
        errno = 0;
        pid_t re = waitpid(-1, &st3, 0);
        printf("nochild echild=%d\n", re == -1 && errno == ECHILD);
    }

    printf("waitstop done\n");
    return 0;
}
