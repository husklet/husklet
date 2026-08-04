// An uncaught fatal-default signal must terminate the guest WITHOUT the engine writing any internal
// diagnostic to the guest's own stderr. A child redirects its stderr into a pipe and raises SIGTERM (default
// action: terminate); the parent reads whatever the child's stderr received. Real Linux writes nothing, so
// the byte count is 0. A translator that emitted an engine-internal "[HLRAISE] ..." line to the guest stderr
// fd on the fatal signal makes this non-empty.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int p[2];
    if (pipe(p) != 0) return 2;
    pid_t pid = fork();
    if (pid == 0) {
        close(p[0]);
        dup2(p[1], 2); // child stderr -> pipe
        close(p[1]);
        raise(SIGTERM); // fatal default: terminate; must print nothing to fd 2
        _exit(0);       // unreachable
    }
    close(p[1]);
    char buf[4096];
    ssize_t total = 0, n;
    while ((n = read(p[0], buf, sizeof buf)) > 0) total += n;
    close(p[0]);
    int st = 0;
    waitpid(pid, &st, 0);
    int sigterm = WIFSIGNALED(st) && WTERMSIG(st) == SIGTERM;
    printf("fatal-raise sigterm=%d stderr_bytes=%ld\n", sigterm, (long)total);
    return 0;
}
