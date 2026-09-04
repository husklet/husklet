#define _GNU_SOURCE
#include <signal.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "success";
    if (strcmp(mode, "signal") == 0) {
        raise(SIGTERM);
        return 91;
    }
    if (strcmp(mode, "exec") == 0) {
        char *const next[] = {"/bin/product-lifecycle", "success", NULL};
        execv(next[0], next);
        return 92;
    }
    if (strcmp(mode, "nested") == 0) {
        pid_t child = fork();
        if (child < 0) return 93;
        if (child == 0) {
            char *const next[] = {"/bin/product-lifecycle", "success", NULL};
            execv(next[0], next);
            _exit(94);
        }
    }
    return 0;
}
