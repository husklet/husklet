#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

enum { CHURN_ITERATIONS = 8192 + 32 };

static void fail(const char *operation) {
    perror(operation);
    _exit(70);
}

static void write_all(const char *text) {
    size_t length = strlen(text);
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, text, length);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) fail("write");
        text += written;
        length -= (size_t)written;
    }
}

static void wait_release(void) {
    char bytes[32];
    for (;;) {
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) fail("read-release");
        if (memchr(bytes, '\n', (size_t)count) != NULL) return;
    }
}

int main(void) {
    write_all("IDENTITY-CHURN-READY\n");
    wait_release();

    for (int iteration = 0; iteration < CHURN_ITERATIONS; ++iteration) {
        pid_t child = fork();
        if (child < 0) {
            dprintf(STDERR_FILENO, "identity-churn iteration=%d fork errno=%d\n", iteration, errno);
            fail("fork-churn");
        }
        if (child == 0) {
            if (setsid() != getpid()) fail("setsid-churn");
            if (getsid(0) != getpid() || getpgid(0) != getpid()) fail("typed-identity-churn");
            _exit(0);
        }
        int status = 0;
        while (waitpid(child, &status, 0) < 0) {
            if (errno == EINTR) continue;
            dprintf(STDERR_FILENO, "identity-churn iteration=%d waitpid child=%d errno=%d\n", iteration, (int)child,
                    errno);
            fail("waitpid-churn");
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) fail("child-status-churn");
        errno = 0;
        if (kill(child, 0) != -1 || errno != ESRCH) fail("reaped-identity-churn");
    }

    write_all("IDENTITY-CHURN-COMPLETE\n");
    return 0;
}
