// execve(2) through a symlinked program: the kernel follows the link chain, resolving a RELATIVE target
// against the directory holding the LINK (/bin/echo -> ../lib/coreutils/echo), an absolute target against
// the root, and a link-to-link chain to its end. Only a dangling link is ENOENT. The same binary is also
// registered as the INITIAL process behind a symlink (exec-symlink-entry), which exercises the launch-side
// executable open rather than this guest-side execve.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static char self[4096];

// Copy this binary to `dst` (0755): the exec targets must be real, independent files, not this image.
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

// execve in a child so a successful exec cannot disturb the harness; the child exits 42 on success.
static const char *attempt(const char *path) {
    pid_t child = fork();
    int status = 0;
    if (child == 0) {
        char *arguments[] = {(char *)path, "child", NULL};
        char *environment[] = {NULL};
        execve(path, arguments, environment);
        _exit(errno == ENOENT ? 71 : errno == EACCES ? 72 : errno == ELOOP ? 73 : 70);
    }
    if (child < 0) return "fork-failed";
    waitpid(child, &status, 0);
    if (!WIFEXITED(status)) return "signalled";
    switch (WEXITSTATUS(status)) {
    case 42: return "ok";
    case 71: return "ENOENT";
    case 72: return "EACCES";
    case 73: return "ELOOP";
    default: return "other";
    }
}

static void link_at(const char *target, const char *path) {
    unlink(path);
    if (symlink(target, path) != 0) printf("symlink-failed %s\n", path);
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "child") == 0) return 42;
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
    char root[3200], bin[3300], lib[3300], deep[3400], target[3600], path[3700];
    snprintf(root, sizeof root, "%s/hl_exec_symlink", base);
    snprintf(bin, sizeof bin, "%s/bin", root);
    snprintf(lib, sizeof lib, "%s/lib", root);
    snprintf(deep, sizeof deep, "%s/deep", lib);
    mkdir(root, 0755);
    mkdir(bin, 0755);
    mkdir(lib, 0755);
    mkdir(deep, 0755);
    snprintf(target, sizeof target, "%s/prog", deep);
    char sibling[3600];
    snprintf(sibling, sizeof sibling, "%s/prog", bin);
    if (!copy_self(target) || !copy_self(sibling)) {
        printf("copy-failed\n");
        return 1;
    }

    snprintf(path, sizeof path, "%s/single", bin);
    link_at("prog", path); // same directory
    snprintf(path, sizeof path, "%s/multi", bin);
    link_at("../lib/deep/prog", path); // crosses directories via ..
    snprintf(path, sizeof path, "%s/absolute", bin);
    link_at(target, path);
    snprintf(path, sizeof path, "%s/chained", bin);
    link_at("multi", path); // link to a link
    snprintf(path, sizeof path, "%s/dangling", bin);
    link_at("../lib/deep/absent", path);

    static const char *names[] = {"single", "multi", "absolute", "chained", "dangling"};
    for (unsigned index = 0; index < sizeof names / sizeof names[0]; index++) {
        snprintf(path, sizeof path, "%s/%s", bin, names[index]);
        printf("exec-symlink %s %s\n", names[index], attempt(path));
    }
    fflush(stdout);

    for (unsigned index = 0; index < sizeof names / sizeof names[0]; index++) {
        snprintf(path, sizeof path, "%s/%s", bin, names[index]);
        unlink(path);
    }
    unlink(sibling);
    unlink(target);
    rmdir(deep);
    rmdir(lib);
    rmdir(bin);
    rmdir(root);
    return 0;
}
