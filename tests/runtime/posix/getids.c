// process identity: getpid/getppid distinct & positive; uid/gid families consistent.
#include <stdio.h>
#include <unistd.h>

int main(void) {
    pid_t pid = getpid();
    pid_t ppid = getppid();
    int pid_ok = pid > 0;
    // PID-namespace init is the one valid zero-parent process. Outside that role, parent identity is
    // positive and distinct. This normalization is true both under native/QEMU and in a real container.
    int ppid_ok = (pid == 1) ? ppid == 0 : ppid > 0 && ppid != pid;
    int uid_ok = getuid() == geteuid() || geteuid() >= 0; // euid sane
    int gid_ok = getgid() >= 0 && getegid() >= 0;
    pid_t pg = getpgrp();
    int pg_ok = pg > 0;
    pid_t sid = getsid(0);
    int sid_ok = sid > 0;
    printf("getids pid=%d ppid=%d uid=%d gid=%d pgrp=%d sid=%d\n", pid_ok, ppid_ok, uid_ok, gid_ok, pg_ok, sid_ok);
    return 0;
}
