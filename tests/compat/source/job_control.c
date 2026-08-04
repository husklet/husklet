#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    errno = 0;
    int negative_group = setpgid(0, -1) == -1 && errno == EINVAL;
    errno = 0;
    int negative_pgid = getpgid(-1) == -1 && errno == ESRCH;
    errno = 0;
    int negative_sid = getsid(-1) == -1 && errno == ESRCH;
    int self = getpgid(0) > 0 && getsid(0) > 0;

    int release[2];
    if (pipe(release)) return 20;
    pid_t child = fork();
    if (child == 0) {
        close(release[1]);
        char token;
        _exit(read(release[0], &token, 1) == 1 ? 0 : 30);
    }
    close(release[0]);
    int child_group = setpgid(child, 0) == 0 && getpgid(child) == child;
    int released = write(release[1], "x", 1) == 1;
    int status = 0;
    waitpid(child, &status, 0);
    int waited = released && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    errno = 0;
    int reaped = getpgid(child) == -1 && errno == ESRCH;

    pid_t session_child = fork();
    if (session_child == 0) {
        pid_t sid = setsid();
        _exit(sid == getpid() && getpgid(0) == getpid() && getsid(0) == getpid() ? 0 : 40);
    }
    waitpid(session_child, &status, 0);
    int session = WIFEXITED(status) && WEXITSTATUS(status) == 0;

    pid_t leader = fork();
    if (leader == 0) {
        if (setpgid(0, 0) != 0) _exit(50);
        errno = 0;
        _exit(setsid() == -1 && errno == EPERM ? 0 : 51);
    }
    waitpid(leader, &status, 0);
    int leader_eperm = WIFEXITED(status) && WEXITSTATUS(status) == 0;

    printf("job_control self=%d neg_group=%d neg_pgid=%d neg_sid=%d child_group=%d waited=%d reaped=%d session=%d leader_eperm=%d\n",
        self, negative_group, negative_pgid, negative_sid, child_group,
        waited, reaped, session, leader_eperm);
    return 0;
}
