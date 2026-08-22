#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

enum { EXECUTED = 42, EXEC_DENIED = 100 + EACCES };

static int copy_self(const char *path, mode_t mode, uid_t uid, gid_t gid) {
    int input = open("/proc/self/exe", O_RDONLY);
    int output = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    char bytes[16384];
    int ok = input >= 0 && output >= 0;
    for (;;) {
        ssize_t count = ok ? read(input, bytes, sizeof bytes) : -1;
        if (count == 0) break;
        if (count < 0) {
            ok = 0;
            break;
        }
        for (ssize_t offset = 0; offset < count;) {
            ssize_t written = write(output, bytes + offset, (size_t)(count - offset));
            if (written <= 0) {
                ok = 0;
                break;
            }
            offset += written;
        }
        if (!ok) break;
    }
    if (input >= 0) close(input);
    if (output >= 0 && close(output) != 0) ok = 0;
    return ok && chown(path, uid, gid) == 0 && chmod(path, mode) == 0;
}

static int run_case(const char *name, mode_t mode, uid_t owner, gid_t group, uid_t uid, gid_t gid, const gid_t *groups,
                    size_t group_count, int expect_exec) {
    char path[256];
    snprintf(path, sizeof path, "/tmp/husklet-exec-dac-%s", name);
    if (!copy_self(path, mode, owner, group)) return 0;
    pid_t child = fork();
    if (child == 0) {
        if (setgroups(group_count, groups) != 0 || setresgid(gid, gid, gid) != 0 || setresuid(uid, uid, uid) != 0)
            _exit(99);
        char *const argv[] = {(char *)path, (char *)"--probe", NULL};
        execve(path, argv, environ);
        _exit(errno == EACCES ? EXEC_DENIED : 98);
    }
    int status = 0;
    int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) &&
             WEXITSTATUS(status) == (expect_exec ? EXECUTED : EXEC_DENIED);
    unlink(path);
    return ok;
}

static int shebang_interpreter_denied(void) {
    const char *interpreter = "/tmp/husklet-exec-dac-interpreter";
    const char *script = "/tmp/husklet-exec-dac-script";
    if (!copy_self(interpreter, 0644, 1002, 3000)) return 0;
    int descriptor = open(script, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    int ok = descriptor >= 0 && dprintf(descriptor, "#!%s --probe\n", interpreter) > 0 && close(descriptor) == 0 &&
             chown(script, 1002, 3000) == 0 && chmod(script, 0755) == 0;
    if (!ok) return 0;
    pid_t child = fork();
    if (child == 0) {
        gid_t groups[] = {3000};
        if (setgroups(1, groups) != 0 || setresgid(3000, 3000, 3000) != 0 || setresuid(1002, 1002, 1002) != 0)
            _exit(99);
        char *const argv[] = {(char *)script, NULL};
        execve(script, argv, environ);
        _exit(errno == EACCES ? EXEC_DENIED : 98);
    }
    int status = 0;
    ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == EXEC_DENIED;
    unlink(script);
    unlink(interpreter);
    return ok;
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--probe")) return EXECUTED;
    gid_t primary[] = {3000};
    gid_t supplementary[] = {2001};
    int ok = 1;
    ok &= run_case("owner-allow", 0100, 1001, 2001, 1001, 3000, primary, 1, 1);
    /* Matching owner selects owner bits; group/other execute cannot rescue it. */
    ok &= run_case("owner-deny", 0011, 1001, 2001, 1001, 2001, supplementary, 1, 0);
    ok &= run_case("group-allow", 0010, 1001, 2001, 1002, 2001, primary, 1, 1);
    ok &= run_case("supplementary-allow", 0010, 1001, 2001, 1002, 3000, supplementary, 1, 1);
    /* Matching supplementary group selects group bits; other execute cannot rescue it. */
    ok &= run_case("group-deny", 0001, 1001, 2001, 1002, 3000, supplementary, 1, 0);
    ok &= run_case("other-allow", 0001, 1001, 2001, 1002, 3000, primary, 1, 1);
    ok &= run_case("root-no-bits", 0000, 1001, 2001, 0, 0, primary, 1, 0);
    ok &= run_case("root-some-bit", 0001, 1001, 2001, 0, 0, primary, 1, 1);
    ok &= shebang_interpreter_denied();
    printf("exec-dac ok=%d\n", ok);
    return ok ? 0 : 1;
}
