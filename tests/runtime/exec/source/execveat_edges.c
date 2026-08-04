#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif
#ifndef AT_SYMLINK_NOFOLLOW
#define AT_SYMLINK_NOFOLLOW 0x100
#endif

static char self[4096];

static int copy_self(const char *path) {
    char bytes[65536];
    int input = open(self, O_RDONLY);
    int output = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    ssize_t count;
    int ok = input >= 0 && output >= 0;
    while (ok && (count = read(input, bytes, sizeof bytes)) > 0)
        ok = write(output, bytes, (size_t)count) == count;
    if (input >= 0) close(input);
    if (output >= 0) close(output);
    return ok && chmod(path, 0755) == 0;
}

static int reap(pid_t child) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status)
        ? WEXITSTATUS(status) : 255;
}

static int attempt(int directory, const char *path, int flags) {
    pid_t child = fork();
    if (child == 0) {
        char *arguments[] = {self, (char *)"probe", NULL};
        char *environment[] = {NULL};
        syscall(SYS_execveat, directory, path, arguments, environment, flags);
        _exit(errno & 0x7f);
    }
    return child < 0 ? 255 : reap(child);
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "probe") == 0) return 42;
    ssize_t length = readlink("/proc/self/exe", self, sizeof self - 1);
    if (length <= 0) snprintf(self, sizeof self, "%s", argv[0]);
    else self[length] = 0;
    char root[128], program[160], link[160];
    snprintf(root, sizeof root, "/tmp/hl_execveat_%d", (int)getpid());
    snprintf(program, sizeof program, "%s/program", root);
    snprintf(link, sizeof link, "%s/link", root);
    mkdir(root, 0755);
    int copied = copy_self(program);
    int directory = open(root, O_RDONLY | O_DIRECTORY);
    int descriptor = open(program, O_PATH);
    int linked = symlink("program", link) == 0;
    int dirfd = copied && attempt(directory, "program", 0) == 42;
    int empty = descriptor >= 0 && attempt(descriptor, "", AT_EMPTY_PATH) == 42;
    int nofollow = linked && attempt(directory, "link", AT_SYMLINK_NOFOLLOW) == ELOOP;
    int follow = linked && attempt(directory, "link", 0) == 42;
    int bad = attempt(directory, "program", 0x80000000) == EINVAL;
    printf("dirfd=%d empty_path=%d nofollow=%d follow=%d bad_flags=%d\n",
           dirfd, empty, nofollow, follow, bad);
    close(descriptor); close(directory); unlink(link); unlink(program); rmdir(root);
    return 0;
}
