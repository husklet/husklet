#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

enum { CHILDREN = 32 };

int main(int argc, char **argv) {
    int gate[2];
    pid_t children[CHILDREN];
    int count = 0;
    int exhausted = 0;
    int require_exhaustion = argc == 2 && strcmp(argv[1], "--exhaust") == 0;
    if (pipe(gate) != 0) return 1;
    for (; count < CHILDREN; ++count) {
        pid_t child = fork();
        if (child < 0) {
            if (errno != ENOMEM && errno != ENOSPC) return 2;
            exhausted = 1;
            break;
        }
        if (child == 0) {
            char byte;
            close(gate[1]);
            while (read(gate[0], &byte, 1) < 0 && errno == EINTR) {}
            _exit(0);
        }
        children[count] = child;
    }
    close(gate[0]);
    close(gate[1]);
    for (int index = 0; index < count; ++index) {
        int status;
        while (waitpid(children[index], &status, 0) < 0 && errno == EINTR) {}
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 3;
    }
    if (require_exhaustion != exhausted) return 4;
    puts("fdvis-capacity ok=1");
    return 0;
}
