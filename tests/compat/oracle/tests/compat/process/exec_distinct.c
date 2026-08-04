// execve(2) of a program OTHER than the one the process was launched with: `sh -c 'exec bash'` is the
// shape every shell uses, and it must run the named image, not report ENOENT. Covers a plain copy, the
// same copy reached as a #! interpreter, and a missing path (still ENOENT).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static char self[4096];

static int copy_self(const char *dst) {
    char buffer[65536];
    ssize_t got;
    int in = open(self, O_RDONLY);
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    int ok = in >= 0 && out >= 0;
    while (ok && (got = read(in, buffer, sizeof buffer)) > 0)
        if (write(out, buffer, (size_t)got) != got) ok = 0;
    if (in >= 0) close(in);
    if (out >= 0) close(out);
    return ok && chmod(dst, 0755) == 0;
}

// The child marker is the LAST argument: a #! interpreter is entered as (interpreter, script, args...).
static const char *attempt(const char *path) {
    pid_t child = fork();
    int status = 0;
    if (child == 0) {
        char *arguments[] = {(char *)path, "child", NULL};
        char *environment[] = {NULL};
        execve(path, arguments, environment);
        _exit(errno == ENOENT ? 71 : errno == EACCES ? 72 : errno == ENOEXEC ? 74 : 70);
    }
    if (child < 0) return "fork-failed";
    waitpid(child, &status, 0);
    if (!WIFEXITED(status)) return "signalled";
    switch (WEXITSTATUS(status)) {
    case 42: return "ok";
    case 71: return "ENOENT";
    case 72: return "EACCES";
    case 74: return "ENOEXEC";
    default: return "other";
    }
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[argc - 1], "child") == 0) return 42;
    ssize_t length = readlink("/proc/self/exe", self, sizeof self - 1);
    if (length <= 0)
        snprintf(self, sizeof self, "%s", argv[0]);
    else
        self[length] = 0;

    char base[3072];
    if (getcwd(base, sizeof base) == NULL) {
        printf("getcwd-failed\n");
        return 1;
    }
    char root[3200], program[3400], script[3400], missing[3400];
    snprintf(root, sizeof root, "%s/hl_exec_distinct", base);
    mkdir(root, 0755);
    snprintf(program, sizeof program, "%s/program", root);
    snprintf(script, sizeof script, "%s/script", root);
    snprintf(missing, sizeof missing, "%s/absent", root);
    if (!copy_self(program)) {
        printf("copy-failed\n");
        return 1;
    }
    int fd = open(script, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    char shebang[3500];
    int shebang_size = snprintf(shebang, sizeof shebang, "#!%s\n", program);
    int wrote = fd >= 0 && write(fd, shebang, (size_t)shebang_size) == shebang_size;
    if (fd >= 0) close(fd);
    if (!wrote || chmod(script, 0755) != 0) {
        printf("script-failed\n");
        return 1;
    }

    printf("exec-distinct program %s\n", attempt(program));
    printf("exec-distinct script %s\n", attempt(script));
    printf("exec-distinct missing %s\n", attempt(missing));
    fflush(stdout);

    unlink(script);
    unlink(program);
    rmdir(root);
    return 0;
}
