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
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/file.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define N 200

struct worker_diag {
    uint64_t device;
    uint64_t inode;
    int pid;
    int locks;
    int reads;
    int writes;
};

struct shared_diag {
    _Atomic int inside;
    _Atomic int overlap;
    struct worker_diag worker[2];
};

static int lock_whole(int fd, int type) {
    struct flock fl = {.l_type = (short)type, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    int result;
    do result = fcntl(fd, F_SETLKW, &fl); while (result < 0 && errno == EINTR);
    return result;
}

static void worker(const char *path, struct shared_diag *diag, int index) {
    int fd = open(path, O_RDWR); // a fresh open in this process -> distinct fd, same (dev,ino) lock domain
    if (fd < 0) _exit(10);
    struct stat status;
    if (fstat(fd, &status) != 0) _exit(19);
    diag->worker[index].device = (uint64_t)status.st_dev;
    diag->worker[index].inode = (uint64_t)status.st_ino;
    diag->worker[index].pid = (int)getpid();
    for (int i = 0; i < N; i++) {
        if (lock_whole(fd, F_WRLCK) != 0) _exit(11);
        diag->worker[index].locks++;
        if (atomic_fetch_add_explicit(&diag->inside, 1, memory_order_acq_rel) != 0)
            atomic_store_explicit(&diag->overlap, 1, memory_order_release);
        char buf[32];
        if (lseek(fd, 0, SEEK_SET) != 0) _exit(12);
        int n = (int)read(fd, buf, sizeof buf - 1);
        if (n <= 0) _exit(13);
        diag->worker[index].reads++;
        buf[n] = 0;
        long v = atol(buf) + 1;
        char out[32];
        int len = snprintf(out, sizeof out, "%ld", v);
        if (lseek(fd, 0, SEEK_SET) != 0) _exit(14);
        if (ftruncate(fd, 0) != 0) _exit(15);
        if (write(fd, out, (size_t)len) != len) _exit(16);
        diag->worker[index].writes++;
        atomic_fetch_sub_explicit(&diag->inside, 1, memory_order_acq_rel);
        if (lock_whole(fd, F_UNLCK) != 0) _exit(17);
    }
    if (close(fd) != 0) _exit(18);
    _exit(0);
}

int main(void) {
    struct shared_diag *diag = mmap(NULL, sizeof(*diag), PROT_READ | PROT_WRITE,
                                    MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (diag == MAP_FAILED) return 3;
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
    if (a < 0) { perror("poslk fork a"); return 2; }
    if (a == 0) worker(path, diag, 0);
    pid_t b = fork();
    if (b < 0) { perror("poslk fork b"); return 2; }
    if (b == 0) worker(path, diag, 1);
    int ast = 0, bst = 0;
    if (waitpid(a, &ast, 0) != a || waitpid(b, &bst, 0) != b || !WIFEXITED(ast) || WEXITSTATUS(ast) != 0 ||
        !WIFEXITED(bst) || WEXITSTATUS(bst) != 0) {
        fprintf(stderr, "poslk workers a=0x%x b=0x%x\n", ast, bst);
        unlink(path);
        return 2;
    }

    int rf = open(path, O_RDONLY);
    char buf[32];
    int n = (int)read(rf, buf, sizeof buf - 1);
    buf[n > 0 ? n : 0] = 0;
    close(rf);
    long final = atol(buf);
    unlink(path);

    if (final != 2 * N) {
        fprintf(stderr,
                "poslk diagnostic final=%ld overlap=%d inside=%d "
                "a={pid=%d dev=%llu ino=%llu lock=%d read=%d write=%d} "
                "b={pid=%d dev=%llu ino=%llu lock=%d read=%d write=%d}\n",
                final, atomic_load_explicit(&diag->overlap, memory_order_acquire),
                atomic_load_explicit(&diag->inside, memory_order_acquire),
                diag->worker[0].pid, (unsigned long long)diag->worker[0].device,
                (unsigned long long)diag->worker[0].inode, diag->worker[0].locks,
                diag->worker[0].reads, diag->worker[0].writes,
                diag->worker[1].pid, (unsigned long long)diag->worker[1].device,
                (unsigned long long)diag->worker[1].inode, diag->worker[1].locks,
                diag->worker[1].reads, diag->worker[1].writes);
    }

    printf("poslk final=%ld noloss=%d getlk=%d indep=%d\n", final, final == 2 * N, getlk_seen,
           flock_ok && fcntl_ok);
    munmap(diag, sizeof(*diag));
    return final == 2 * N && getlk_seen && flock_ok && fcntl_ok ? 0 : 2;
}
