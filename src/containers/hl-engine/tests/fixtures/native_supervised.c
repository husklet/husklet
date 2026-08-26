#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <signal.h>

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "output")) {
        fputs("native-supervised", stdout);
        return 23;
    }
    if (argc > 1 && !strcmp(argv[1], "descendant")) {
        pid_t child = fork();
        if (child < 0) return 31;
        if (child == 0) {
            errno = 0;
            long result = syscall(SYS_getpid);
            _exit(result == -1 && errno == ENOSYS ? 37 : 38);
        }
        int status;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 37) return 32;
        fputs("descendant-supervised", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "orphan")) {
        pid_t child = fork();
        if (child < 0) return 41;
        if (child == 0) {
            usleep(20000);
            errno = 0;
            long result = syscall(SYS_getpid);
            if (result == -1 && errno == ENOSYS && write(1, "orphan-supervised", 17) != 17) _exit(43);
            _exit(0);
        }
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "environment")) {
        const char *value = getenv("NATIVE_SUPERVISED_ENV");
        const char *leak = getenv("HOME");
        if (value == NULL || strcmp(value, "line1\nline2\\tail") || leak != NULL) return 42;
        fputs("environment-exact", stdout);
    }
    if (argc > 1 && !strcmp(argv[1], "streams")) {
        char input[5] = {0};
        if (read(0, input, 4) != 4 || memcmp(input, "pipe", 4)) return 51;
        struct iovec output[] = {{"write", 5}, {"v", 1}};
        if (writev(1, output, 2) != 6) return 52;
        if (write(2, "stderr", 6) != 6) return 53;
        if (dup2(1, 7) != 7 || write(7, "-dup", 4) != 4) return 54;
    }
    if (argc > 1 && !strcmp(argv[1], "signal")) raise(SIGTERM);
    if (argc > 1 && !strcmp(argv[1], "sendmsg-denied")) {
        int pair[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) return 61;
        char byte = 1;
        struct iovec vector = {&byte, 1};
        struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1};
        errno = 0;
        if (sendmsg(pair[0], &message, 0) != -1 || errno != EPERM) return 62;
        fputs("sendmsg-denied", stdout);
    }
    return 0;
}
