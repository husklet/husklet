#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

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
    return 0;
}
