#include <errno.h>
#include <fcntl.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int park(void) {
    struct timespec interval = {.tv_nsec = 1000000};
    for (;;)
        if (nanosleep(&interval, NULL) != 0 && errno != EINTR) return 1;
}

static int child(int output, int role) {
    if (role == 3) {
        struct sock_filter allow[] = {BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW)};
        struct sock_fprog program = {.len = 1, .filter = allow};
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 || prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) != 0)
            return 10;
        dprintf(output, "SECCOMP-ARMED %d\n", role);
    } else
        dprintf(output, "CAPTURE-CAPABLE %d\n", role);
    dprintf(output, "READY %d\n", role);
    return park();
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    char path[1024];
    if (snprintf(path, sizeof path, "%s.output", argv[1]) >= (int)sizeof path) return 3;
    int output = open(path, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (output < 0) return 4;
    for (int role = 2; role <= 3; ++role) {
        pid_t process = fork();
        if (process < 0) return 5;
        if (process == 0) return child(output, role);
    }
    dprintf(output, "CAPTURE-CAPABLE 1\nREADY 1\n");
    int status;
    while (wait(&status) >= 0 || errno == EINTR) {}
    return 0;
}
