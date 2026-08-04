// execve / execveat(by path) / fexecve (execveat + AT_EMPTY_PATH) self-re-exec, all landing in the
// same binary in "child" mode, with argv/env passthrough verified. Deterministic derived booleans only.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

// child mode: argv = [self, "child", MODE], env carries MARK=magic; report what we saw via exit code.
static int child_main(int argc, char **argv) {
    int argv_ok = argc == 3 && strcmp(argv[1], "child") == 0;
    const char *mark = getenv("EXEC_MARK");
    int env_ok = mark != NULL && strcmp(mark, "magic42") == 0;
    // encode findings into the exit code: bit0 argv, bit1 env, bit2 mode string present
    int code = (argv_ok ? 1 : 0) | (env_ok ? 2 : 0) | (argv[2] && argv[2][0] ? 4 : 0);
    return code; // expected 7 for a clean exec
}

static int reap(pid_t p) {
    int st = 0;
    if (waitpid(p, &st, 0) != p) return -1;
    return WIFEXITED(st) ? WEXITSTATUS(st) : -2;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "child") == 0) return child_main(argc, argv);

    char *self = argv[0];
    char *cenv[] = { (char *)"EXEC_MARK=magic42", NULL };

    // 1. plain execve
    pid_t a = fork();
    if (a == 0) {
        char *cargv[] = { self, (char *)"child", (char *)"execve", NULL };
        execve(self, cargv, cenv);
        _exit(100);
    }
    printf("execve code=%d\n", reap(a));

    // 2. execveat by path (AT_FDCWD)
    pid_t b = fork();
    if (b == 0) {
        char *cargv[] = { self, (char *)"child", (char *)"execveat", NULL };
        syscall(SYS_execveat, AT_FDCWD, self, cargv, cenv, 0);
        _exit(101);
    }
    printf("execveat code=%d\n", reap(b));

    // 3. fexecve: open the binary, exec via the fd (execveat + AT_EMPTY_PATH)
    pid_t c = fork();
    if (c == 0) {
        int fd = open(self, O_RDONLY | O_CLOEXEC);
        if (fd < 0) _exit(102);
        char *cargv[] = { self, (char *)"child", (char *)"fexecve", NULL };
        fexecve(fd, cargv, cenv);
        _exit(103);
    }
    printf("fexecve code=%d\n", reap(c));

    printf("exec_self done\n");
    return 0;
}
