// Closing a descriptor must release its byte-range locks with no explicit F_UNLCK:
// final close of an open-file description drops its F_OFD_SETLK ranges, plain close drops
// that process's POSIX ranges, and close_range does the same for a whole span.
// POSIX ranges never conflict with their own holder, so every POSIX probe runs in a forked
// child, and each check is paired with a still-held positive control to keep it non-vacuous.
// Linux -> deterministic golden verdict on every engine.
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

static struct flock make(short type, off_t start, off_t len) {
    struct flock fl;
    memset(&fl, 0, sizeof fl);
    fl.l_type = type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fl;
}

// Returns 1 when a separate process sees [start,len) as lock-free, 0 when blocked.
static int free_to_others(const char *path, off_t start, off_t len) {
    pid_t pid = fork();
    if (pid == 0) {
        int fd = open(path, O_RDWR);
        struct flock q = make(F_WRLCK, start, len);
        int clear = fd >= 0 && fcntl(fd, F_GETLK, &q) == 0 && q.l_type == F_UNLCK;
        _exit(clear ? 0 : 1);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_lockclose_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int probe = open(path, O_CREAT | O_RDWR, 0644);
    ftruncate(probe, 8192);

    // OFD lock, then close with no F_UNLCK. A dup keeps the description alive, so the lock
    // must survive the first close and vanish only when the last descriptor goes away.
    int a = open(path, O_RDWR);
    struct flock w = make(F_WRLCK, 0, 1024);
    int ofd_taken = fcntl(a, F_OFD_SETLK, &w) == 0;
    int a2 = dup(a);
    close(a);
    struct flock q = make(F_WRLCK, 0, 1024);
    int held_after_dup_close = fcntl(probe, F_OFD_GETLK, &q) == 0 && q.l_type == F_WRLCK;
    close(a2);
    struct flock q2 = make(F_WRLCK, 0, 1024);
    int ofd_released = fcntl(probe, F_OFD_GETLK, &q2) == 0 && q2.l_type == F_UNLCK;

    // POSIX lock dropped by a plain close with no F_UNLCK. The pre-close probe is the
    // positive control: it must observe the conflict, or the post-close probe proves nothing.
    int b = open(path, O_RDWR);
    struct flock pw = make(F_WRLCK, 2048, 1024);
    int posix_taken = fcntl(b, F_SETLK, &pw) == 0;
    int posix_seen = !free_to_others(path, 2048, 1024);
    close(b);
    int posix_released = free_to_others(path, 2048, 1024);

    // POSIX locks on several descriptors dropped by one close_range.
    int c = open(path, O_RDWR);
    int d = open(path, O_RDWR);
    struct flock r1 = make(F_WRLCK, 4096, 512);
    struct flock r2 = make(F_WRLCK, 5120, 512);
    int span_taken = fcntl(c, F_SETLK, &r1) == 0 && fcntl(d, F_SETLK, &r2) == 0;
    int span_seen = !free_to_others(path, 4096, 512) && !free_to_others(path, 5120, 512);
    int lo = c < d ? c : d;
    int hi = c < d ? d : c;
    int ranged = syscall(SYS_close_range, (unsigned)lo, (unsigned)hi, 0u) == 0;
    int span_released = free_to_others(path, 4096, 512) && free_to_others(path, 5120, 512);

    close(probe);
    unlink(path);
    rmdir(dir);
    printf("lock-close ofd=%d dup=%d ofdfree=%d posix=%d seen=%d posixfree=%d span=%d spanseen=%d "
           "ranged=%d spanfree=%d\n",
           ofd_taken, held_after_dup_close, ofd_released, posix_taken, posix_seen, posix_released, span_taken,
           span_seen, ranged, span_released);
    return 0;
}
