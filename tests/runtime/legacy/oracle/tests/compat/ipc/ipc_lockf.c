// lockf() POSIX record lock across a fork: child's F_TLOCK fails while the parent holds F_LOCK on
// the whole file, then succeeds after the parent F_ULOCKs (signaled via a pipe). Portable, golden.
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <errno.h>
#include <time.h>
#include <sys/wait.h>
static int byte_write(int fd, char value) {
    ssize_t result; do result = write(fd, &value, 1); while (result < 0 && errno == EINTR); return result == 1;
}
static int byte_read(int fd) {
    char value; ssize_t result; do result = read(fd, &value, 1); while (result < 0 && errno == EINTR); return result == 1;
}
int main(void) {
    char path[] = "/tmp/hl_lockf_XXXXXX"; int fd = mkstemp(path);
    if (fd < 0 || ftruncate(fd, 100) != 0 || lockf(fd, F_LOCK, 100) != 0) return 1;
    int p[2], ready[2]; if (pipe(p) != 0 || pipe(ready) != 0) return 1;
    pid_t pid = fork();
    if (pid == 0) {
        int cfd = open(path, O_RDWR);
        int blocked = (lockf(cfd, F_TLOCK, 100) < 0 && (errno == EACCES || errno == EAGAIN));
        if (!byte_write(ready[1], 'r') || !byte_read(p[0])) _exit(2);
        int ok = lockf(cfd, F_TLOCK, 100) == 0;
        printf("lockf blocked=%d acquired=%d\n", blocked, ok);
        fflush(stdout);
        close(cfd); _exit(0);
    }
    if (!byte_read(ready[0])) return 1;
    if (lockf(fd, F_ULOCK, 100) != 0) return 1;
    if (!byte_write(p[1], 'g')) return 1;
    waitpid(pid, 0, 0);

    int alias = open(path, O_RDWR);
    if (alias < 0 || lockf(fd, F_LOCK, 100) != 0 || pipe(p) != 0 || pipe(ready) != 0) return 1;
    pid = fork();
    if (pid == 0) {
        int cfd = open(path, O_RDWR);
        int blocked = lockf(cfd, F_TLOCK, 100) < 0 && (errno == EACCES || errno == EAGAIN);
        if (!byte_write(ready[1], 'r') || !byte_read(p[0])) _exit(2);
        int acquired = lockf(cfd, F_TLOCK, 100) == 0;
        printf("lockf close-any=%d acquired=%d\n", blocked, acquired);
        fflush(stdout);
        close(cfd);
        _exit(0);
    }
    if (!byte_read(ready[0]) || close(alias) != 0 || !byte_write(p[1], 'g')) return 1;
    waitpid(pid, 0, 0);
    close(fd); unlink(path);
    return 0;
}
