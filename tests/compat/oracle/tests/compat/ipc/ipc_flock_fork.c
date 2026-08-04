// flock() advisory whole-file lock across a fork: child's LOCK_EX|LOCK_NB fails while the parent
// holds LOCK_EX, then succeeds after the parent unlocks (signaled via a pipe). Portable, golden.
#include <sys/file.h>
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <errno.h>
#include <time.h>
#include <unistd.h>
#include <sys/wait.h>
static int byte_write(int fd, char value) {
    ssize_t result;
    do result = write(fd, &value, 1); while (result < 0 && errno == EINTR);
    return result == 1;
}
static int byte_read(int fd) {
    char value;
    ssize_t result;
    do result = read(fd, &value, 1); while (result < 0 && errno == EINTR);
    return result == 1;
}
int main(void) {
    char path[] = "/tmp/hl_flock_XXXXXX"; int fd = mkstemp(path);
    flock(fd, LOCK_EX);
    int p[2], ready[2]; if (pipe(p) != 0 || pipe(ready) != 0) return 1;
    pid_t pid = fork();
    if (pid == 0) {
        int cfd = open(path, O_RDWR);
        int nb = flock(cfd, LOCK_EX | LOCK_NB);
        int blocked = (nb < 0 && (errno == EWOULDBLOCK || errno == EAGAIN));
        if (!byte_write(ready[1], 'r') || !byte_read(p[0])) _exit(2);
        int ok = flock(cfd, LOCK_EX | LOCK_NB) == 0;
        printf("flock child_blocked=%d child_acquired=%d\n", blocked, ok);
        fflush(stdout);
        close(cfd); _exit(0);
    }
    if (!byte_read(ready[0])) return 1;
    flock(fd, LOCK_UN);
    if (!byte_write(p[1], 'g')) return 1;
    waitpid(pid, 0, 0); close(fd); unlink(path);
    return 0;
}
