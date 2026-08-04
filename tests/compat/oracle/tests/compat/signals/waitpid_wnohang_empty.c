// waitpid(WNOHANG) returns 0 when a child exists but has not yet exited, then reaps it after it
// dies. With no children at all it returns -1/ECHILD.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    errno = 0;
    pid_t none = waitpid(-1, NULL, WNOHANG);
    int echild = none == -1 && errno == ECHILD;

    pid_t pid = fork();
    if (pid == 0) { usleep(150 * 1000); _exit(4); }

    usleep(30 * 1000);
    pid_t not_yet = waitpid(pid, NULL, WNOHANG); // child still alive -> 0
    int zero = not_yet == 0;

    int st = 0;
    pid_t reaped = waitpid(pid, &st, 0);
    int ok = reaped == pid && WIFEXITED(st) && WEXITSTATUS(st) == 4;

    printf("waitpid_wnohang_empty echild=%d zero_while_alive=%d reaped=%d\n", echild, zero, ok);
    return 0;
}
