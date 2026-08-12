#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static int child_sees_lock(int descriptor, off_t start, off_t length) {
    pid_t child = fork();
    if (child == 0) {
        struct flock lock = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = start, .l_len = length};
        _exit(fcntl(descriptor, F_GETLK, &lock) == 0 && lock.l_type != F_UNLCK ? 1 : 0);
    }
    int status = 0;
    return child >= 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

int main(void) {
    char path[] = "/tmp/hl-typed-lock-XXXXXX";
    int first = mkstemp(path);
    if (first < 0 || ftruncate(first, 16) != 0) return 1;
    int second = open(path, O_RDWR);
    if (second < 0 || lseek(first, 4, SEEK_SET) != 4) return 2;

    struct flock current = {.l_type = F_WRLCK, .l_whence = SEEK_CUR, .l_start = 0, .l_len = 4};
    int current_ok = fcntl(first, F_SETLK, &current) == 0 && child_sees_lock(second, 4, 4) == 1;
    close(first); /* POSIX: any close by this process drops all of its locks on the inode. */
    int close_ok = child_sees_lock(second, 4, 4) == 0;

    struct flock end = {.l_type = F_WRLCK, .l_whence = SEEK_END, .l_start = -4, .l_len = 4};
    int end_ok = fcntl(second, F_SETLK, &end) == 0 && child_sees_lock(second, 12, 4) == 1;
    struct flock invalid = {.l_type = F_WRLCK, .l_whence = 9, .l_start = 0, .l_len = 1};
    errno = 0;
    int invalid_ok = fcntl(second, F_SETLK, &invalid) == -1 && errno == EINVAL;
    printf("typed-lock current=%d close=%d end=%d invalid=%d\n", current_ok, close_ok, end_ok, invalid_ok);
    close(second);
    unlink(path);
    return current_ok && close_ok && end_ok && invalid_ok ? 0 : 3;
}
