// #340: in-engine POSIX advisory byte-range lock manager -- CROSS-PROCESS correctness.
// Three independent guarantees, one deterministic golden line:
//   (1) MUTUAL EXCLUSION / no lost updates: two child PROCESSES each run N read-increment-write cycles on a
//       shared counter file, each cycle serialized by a whole-file fcntl F_SETLKW *write* lock. Correct
//       cross-process locking (they are separate host processes sharing the engine's lock table) forces the
//       final counter to exactly 2*N -- a broken/no-op lock manager races and loses updates (< 2*N).
//   (2) F_GETLK reports a conflicting holder ACROSS processes (parent holds a write lock; a child sees it).
//   (3) flock(2) and fcntl POSIX record locks are INDEPENDENT lock spaces (#237): one process holds BOTH an
//       exclusive flock AND an exclusive fcntl write lock on the same fd at once, with no self-conflict.
// Portable POSIX, golden-checked -> runs on both Linux engines and native-on-macOS (same answer everywhere).
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/file.h>
#include <sys/wait.h>
#include <unistd.h>

#define N 200

static void lock_whole(int fd, int type) {
    struct flock fl = {.l_type = (short)type, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    while (fcntl(fd, F_SETLKW, &fl) < 0 && errno == EINTR) {
    }
}

static void worker(const char *path) {
    int fd = open(path, O_RDWR); // a fresh open in this process -> distinct fd, same (dev,ino) lock domain
    for (int i = 0; i < N; i++) {
        lock_whole(fd, F_WRLCK); // exclusive: only one worker inside the critical section at a time
        char buf[32];
        lseek(fd, 0, SEEK_SET);
        int n = (int)read(fd, buf, sizeof buf - 1);
        buf[n > 0 ? n : 0] = 0;
        long v = atol(buf) + 1;
        char out[32];
        int len = snprintf(out, sizeof out, "%ld", v);
        lseek(fd, 0, SEEK_SET);
        if (ftruncate(fd, 0) != 0) { /* ignore */ }
        if (write(fd, out, (size_t)len) != len) { /* ignore */ }
        lock_whole(fd, F_UNLCK);
    }
    close(fd);
    _exit(0);
}

int main(void) {
    char path[] = "/tmp/hl_poslk_XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) { printf("poslk mkstemp fail\n"); return 1; }
    if (write(fd, "0", 1) != 1) { /* ignore */ }

    // (3) flock EX + fcntl WR on the same fd must BOTH succeed (independent lock spaces, #237).
    int flock_ok = (flock(fd, LOCK_EX) == 0);
    struct flock w = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    int fcntl_ok = (fcntl(fd, F_SETLK, &w) == 0);

    // (2) a child sees the parent's fcntl write lock via F_GETLK (cross-process).
    int gp[2];
    if (pipe(gp) != 0) { /* ignore */ }
    pid_t g = fork();
    if (g == 0) {
        close(gp[0]);
        int cfd = open(path, O_RDWR);
        struct flock q = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
        fcntl(cfd, F_GETLK, &q);
        int seen = (q.l_type != F_UNLCK); // parent's lock must be visible as a conflict
        if (write(gp[1], &seen, sizeof seen) != sizeof seen) { /* ignore */ }
        close(cfd);
        _exit(0);
    }
    close(gp[1]);
    int getlk_seen = 0;
    if (read(gp[0], &getlk_seen, sizeof getlk_seen) != sizeof getlk_seen) { /* ignore */ }
    waitpid(g, 0, 0);
    close(gp[0]);

    // release both locks (close would drop the fcntl lock anyway) and reset the counter to 0.
    struct flock u = {.l_type = F_UNLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    fcntl(fd, F_SETLK, &u);
    flock(fd, LOCK_UN);
    lseek(fd, 0, SEEK_SET);
    if (ftruncate(fd, 0) != 0) { /* ignore */ }
    if (write(fd, "0", 1) != 1) { /* ignore */ }
    close(fd);

    // (1) two racing writers, each doing N locked increments.
    pid_t a = fork();
    if (a == 0) worker(path);
    pid_t b = fork();
    if (b == 0) worker(path);
    waitpid(a, 0, 0);
    waitpid(b, 0, 0);

    int rf = open(path, O_RDONLY);
    char buf[32];
    int n = (int)read(rf, buf, sizeof buf - 1);
    buf[n > 0 ? n : 0] = 0;
    close(rf);
    long final = atol(buf);
    unlink(path);

    printf("poslk final=%ld noloss=%d getlk=%d indep=%d\n", final, final == 2 * N, getlk_seen,
           flock_ok && fcntl_ok);
    return 0;
}
