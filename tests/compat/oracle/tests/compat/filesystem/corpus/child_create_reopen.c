#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    char dir[128], mysql[160], path[192], resolved[4096];
    snprintf(dir, sizeof dir, "/tmp/hl_child_create_%d", (int)getpid());
    snprintf(mysql, sizeof mysql, "%s/mysql", dir);
    snprintf(path, sizeof path, "%s/global_priv.MAI", mysql);
    if (mkdir(dir, 0755) != 0 || mkdir(mysql, 0755) != 0) return 1;

    int absent = realpath(path, resolved) == NULL;
    pid_t pid = fork();
    if (pid == 0) {
        int fd = open(path, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0660);
        int ok = fd >= 0 && write(fd, "aria-header", 11) == 11 && fdatasync(fd) == 0 && close(fd) == 0;
        _exit(ok ? 0 : 20);
    }
    int status = 0;
    int created = pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int canonical = realpath(path, resolved) != NULL;
    int fd = open(path, O_RDWR | O_NOFOLLOW | O_CLOEXEC);
    char bytes[12] = {0};
    int reopened = fd >= 0 && pread(fd, bytes, 11, 0) == 11 && memcmp(bytes, "aria-header", 11) == 0;
    if (fd >= 0) close(fd);
    unlink(path);
    rmdir(mysql);
    rmdir(dir);
    printf("child-create absent=%d created=%d canonical=%d reopened=%d\n", absent, created, canonical, reopened);
    return absent && created && canonical && reopened ? 0 : 2;
}
