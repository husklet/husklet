#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int publish(const char *path, const char *mode) {
    int descriptor = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    char message[64];
    int length = snprintf(message, sizeof message, "READY %s\n", mode);
    int failed = descriptor < 0 || length <= 0 || length >= (int)sizeof message ||
                 write(descriptor, message, (size_t)length) != length;
    if (descriptor >= 0 && close(descriptor) != 0) failed = 1;
    return failed ? -1 : 0;
}

static int child_shutdown(int descriptor) {
    if (shutdown(descriptor, SHUT_WR) != 0) return 31;
    return close(descriptor) == 0 ? 0 : 32;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    const char *mode = argv[2];
    int sockets[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) != 0) return 3;
    if (write(sockets[1], "queued-before-checkpoint", 24) != 24) return 4;

    if (strcmp(mode, "dup") == 0) {
        int alias = dup(sockets[0]);
        if (alias < 0 || shutdown(alias, SHUT_WR) != 0 || close(alias) != 0) return 5;
    } else if (strcmp(mode, "fork") == 0) {
        pid_t child = fork();
        if (child < 0) return 6;
        if (child == 0) _exit(child_shutdown(sockets[0]));
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 7;
    } else if (strcmp(mode, "clean") != 0) {
        return 8;
    }

    if (strcmp(mode, "clean") != 0 && close(sockets[1]) != 0) return 11;

    if (publish(argv[1], mode) != 0) return 9;
    struct timespec pause = {.tv_sec = 1};
    for (;;) {
        if (nanosleep(&pause, NULL) != 0 && errno != EINTR) return 10;
    }
}
