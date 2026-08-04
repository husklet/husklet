// POSIX record locks are owned by the process, so a fork'd child sees the parent's lock as its
// own and can relock it, while an unrelated grandchild's lock conflicts; OFD locks (F_OFD_SETLK)
// are owned by the open file description and therefore conflict even with the forking child.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    char path[] = "/tmp/hl-forklock-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    if (ftruncate(fd, 4096)) return 1;
    struct flock fl = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
    int p = fcntl(fd, F_SETLK, &fl);

    int child_relock = -99, child_getlk = -99, child_type = -99;
    pid_t c = fork();
    if (c == 0) {
        struct flock q = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
        int r = fcntl(fd, F_SETLK, &q); // same process lock inherited via same fd? no: new pid, conflicts
        int er = (r == -1) ? errno : 0;
        struct flock g = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
        fcntl(fd, F_GETLK, &g);
        _exit((r == 0 ? 0 : 1) + (er == EAGAIN ? 2 : 0) + (g.l_type == F_WRLCK ? 4 : 0));
    }
    int st = 0;
    waitpid(c, &st, 0);
    child_relock = WEXITSTATUS(st);

    // now the OFD flavour
    struct flock un = {.l_type = F_UNLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
    fcntl(fd, F_SETLK, &un);
    struct flock ofd = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
    int o = fcntl(fd, F_OFD_SETLK, &ofd);
    pid_t c2 = fork();
    if (c2 == 0) {
        struct flock q = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
        int r = fcntl(fd, F_OFD_SETLK, &q); // same open file description: granted (replaces)
        int er = (r == -1) ? errno : 0;
        int fd2 = open(path, O_RDWR);
        struct flock q2 = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 16};
        int r2 = fcntl(fd2, F_OFD_SETLK, &q2); // different description: conflicts
        int er2 = (r2 == -1) ? errno : 0;
        _exit((r == 0 ? 1 : 0) + (er == 0 ? 2 : 0) + (r2 == -1 ? 4 : 0) + (er2 == EAGAIN ? 8 : 0));
    }
    waitpid(c2, &st, 0);
    child_getlk = WEXITSTATUS(st);

    // a non-overlapping range is independent
    struct flock far = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 4096, .l_len = 16};
    int f = fcntl(fd, F_SETLK, &far);
    child_type = f;
    close(fd);
    unlink(path);
    printf("p=%d o=%d childposix=%d childofd=%d far=%d\n", p, o, child_relock, child_getlk, child_type);
    return 0;
}
