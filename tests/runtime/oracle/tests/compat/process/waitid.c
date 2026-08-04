// waitid(P_PID, WEXITED): siginfo reports CLD_EXITED + si_status for a normal exit, and CLD_KILLED
// + the signo for a killed child. Portable POSIX -> golden verdict on every engine.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    // normal exit
    pid_t a = fork();
    if (a == 0) { _exit(5); }
    siginfo_t si;
    si.si_pid = 0;
    waitid(P_PID, a, &si, WEXITED);
    int exited = si.si_code == CLD_EXITED && si.si_status == 5 && si.si_pid == a;

    // killed by signal
    pid_t b = fork();
    if (b == 0) { pause(); _exit(0); }
    usleep(50000);
    kill(b, SIGKILL);
    siginfo_t sk;
    waitid(P_PID, b, &sk, WEXITED);
    int killed = sk.si_code == CLD_KILLED && sk.si_status == SIGKILL;
    printf("waitid exited=%d killed=%d\n", exited, killed);
    return 0;
}
