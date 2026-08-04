#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int child(const char *dir) {
    int root = open(dir, O_PATH | O_DIRECTORY | O_CLOEXEC);
    if (root < 0 || fchdir(root) != 0) return 20;
    int fd = open("./mysql/global_priv.MAI", O_RDWR | O_CLOEXEC);
    char resolved[4096];
    int canonical = realpath("./mysql/global_priv.MAI", resolved) != NULL;
    char bytes[12] = {0};
    int ok = fd >= 0 && pread(fd, bytes, 11, 0) == 11 && memcmp(bytes, "aria-header", 11) == 0;
    if (fd >= 0) close(fd);
    close(root);
    return ok && canonical ? 0 : 21;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "child") == 0) return child(argv[2]);
    char dir[128], mysql[160], path[192];
    snprintf(dir, sizeof dir, "/tmp/hl_fork_reopen_%d", (int)getpid());
    snprintf(mysql, sizeof mysql, "%s/mysql", dir);
    snprintf(path, sizeof path, "%s/global_priv.MAI", mysql);
    if (mkdir(dir, 0755) != 0 || mkdir(mysql, 0755) != 0) return 1;
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0660);
    if (fd < 0 || write(fd, "aria-header", 11) != 11 || fdatasync(fd) != 0 || close(fd) != 0) return 2;
    char resolved[4097];
    if (realpath(path, resolved) == NULL) return 4;
    pid_t pid = fork();
    if (pid == 0) {
        char *args[] = {argv[0], "child", dir, NULL};
        execv(argv[0], args);
        _exit(127);
    }
    int status = 0;
    int ok = pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    unlink(path);
    rmdir(mysql);
    rmdir(dir);
    printf("fork-reopen child=%d\n", ok);
    return ok ? 0 : 3;
}
