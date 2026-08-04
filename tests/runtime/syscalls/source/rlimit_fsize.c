// syscall-compat regression: RLIMIT_FSIZE enforcement. With a soft file-size limit of one page, writing up
// to the limit succeeds, and a write that would exceed it raises SIGXFSZ (and, if caught, the write returns
// EFBIG). Runs the offending write in a child so a raw SIGXFSZ termination is observed deterministically via
// the child's wait status. Arch-neutral: signal number / errno / booleans printed.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    pid_t pid = fork();
    if (pid == 0) {
        char tmpl[] = "/tmp/fsize_XXXXXX";
        int fd = mkstemp(tmpl);
        unlink(tmpl);
        struct rlimit rl = {4096, 4096};
        setrlimit(RLIMIT_FSIZE, &rl);
        signal(SIGXFSZ, SIG_DFL); // default action terminates on overflow
        char buf[4096];
        for (int i = 0; i < 4096; i++) buf[i] = 'x';
        // Fill exactly to the limit: succeeds.
        ssize_t ok = write(fd, buf, 4096);
        // Next byte exceeds the limit: SIGXFSZ terminates the child.
        write(fd, buf, 1);
        _exit(ok == 4096 ? 0 : 1); // only reached if no signal fired (would be an engine bug)
    }
    int status = 0;
    waitpid(pid, &status, 0);
    printf("signaled=%d sig=%d\n", WIFSIGNALED(status), WIFSIGNALED(status) ? WTERMSIG(status) : 0);
    printf("is_sigxfsz=%d\n", WIFSIGNALED(status) && WTERMSIG(status) == SIGXFSZ);
    return 0;
}
