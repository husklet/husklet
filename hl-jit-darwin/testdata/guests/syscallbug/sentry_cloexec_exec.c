// Close-on-exec across a guest execve, under the untrusted-guest SENTRY split. A guest execve stays
// worker-local (the image is re-loaded in place, not a real host execve), so the FD_CLOEXEC descriptors
// the kernel would close must be closed by hand -- and in sentry mode those are VIRTUAL fds owned by the
// sentry process. Before the fix the sentry never swept them, so a pipe write end opened O_CLOEXEC leaked
// past the child's execve: its peer never saw EOF.
//
// Probe: parent makes a CLOEXEC pipe and a PLAIN pipe, forks, and the child execve()s itself into a
// blocking "child" mode. The parent drops its own write ends, so only a CHILD-side leak can keep a pipe
// open. Correct semantics: the CLOEXEC write end is closed by the child's exec (parent reads EOF), the
// PLAIN write end survives (parent's read would block). ok=1 iff both hold. Oracle-diffed vs native, so
// the native Linux kernel's own close-on-exec behaviour is the expected value.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

// child mode: stay alive (holding whatever fds survived our execve) until the parent kills us.
static void child_mode(void) {
    pause();
    _exit(0);
}

// Nonblocking read of a pipe read end, polled for up to ~600ms. Returns 1 iff EOF (every write end is
// closed), 0 if a write end is still open (EAGAIN throughout) or unexpected data arrived.
static int drained_to_eof(int rfd) {
    char b[8];
    for (int i = 0; i < 60; i++) {
        ssize_t r = read(rfd, b, sizeof b);
        if (r == 0) return 1;
        if (r < 0 && errno == EAGAIN) {
            usleep(10000);
            continue;
        }
        return 0; // data, or an unexpected error
    }
    return 0; // never reached EOF -> a write end is still open (leak)
}

int main(int argc, char **argv) {
    if (argc > 1) child_mode();

    int cx[2], pl[2];
    if (pipe2(cx, O_CLOEXEC) != 0 || pipe(pl) != 0) {
        printf("cloexec_exec ok=0 setup\n");
        return 0;
    }

    pid_t ch = fork();
    if (ch == 0) {
        char *av[] = {argv[0], "child", NULL};
        execve("/proc/self/exe", av, environ);
        _exit(127);
    }
    // Parent: release our own write ends so only a child-side leak can keep a pipe open.
    close(cx[1]);
    close(pl[1]);
    fcntl(cx[0], F_SETFL, O_NONBLOCK);
    fcntl(pl[0], F_SETFL, O_NONBLOCK);

    int cloexec_eof = drained_to_eof(cx[0]); // CLOEXEC write end must be swept by the child's exec -> EOF
    // The PLAIN write end must survive the child's exec: with the child still blocked, the read blocks.
    char b[8];
    ssize_t r = read(pl[0], b, sizeof b);
    int plain_open = (r < 0 && errno == EAGAIN);

    kill(ch, SIGKILL);
    waitpid(ch, NULL, 0);
    printf("cloexec_exec ok=%d\n", cloexec_eof && plain_open);
    return 0;
}
