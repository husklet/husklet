// Cross-process FUTEX on a shared-memory object passed by SCM_RIGHTS (NOT fork-inherited) — the exact
// Chrome renderer<->GPU-service command-buffer wakeup: the GPU service creates a shm/memfd, sends the
// handle over Mojo (SCM_RIGHTS), and the peer mmaps it and FUTEX_WAITs on a word inside; the other side
// stores + FUTEX_WAKEs through its OWN independent mapping. The existing futex-shared-key gate only covers
// a FORK-INHERITED memfd (same fd number, same VA lineage); this covers an SCM_RIGHTS-delivered memfd that
// lands at a different fd number in an unrelated process, mmap'd at an independent VA. A lost wake surfaces
// as woke=0 within the timed wait (never a hang).
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long fwait(int *a, int e, const struct timespec *to) { return syscall(SYS_futex, a, FUTEX_WAIT, e, to, NULL, 0); }
static long fwake(int *a, int n) { return syscall(SYS_futex, a, FUTEX_WAKE, n, NULL, NULL, 0); }

static int send_fd(int sock, int fd) {
    char b = 'x';
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
    return sendmsg(sock, &mh, 0) == 1 ? 0 : -1;
}
static int recv_fd(int sock) {
    char b;
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    if (recvmsg(sock, &mh, 0) != 1) return -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    if (!c || c->cmsg_type != SCM_RIGHTS) return -1;
    int fd = -1; memcpy(&fd, CMSG_DATA(c), sizeof fd); return fd;
}

int main(void) {
    int zc[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, zc) != 0) { perror("zc"); return 1; }
    // control pipe: renderer -> browser "parked"
    int rp[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, rp) != 0) { perror("rp"); return 1; }

    pid_t zyg = fork();
    if (zyg < 0) { perror("fork"); return 1; }
    if (zyg == 0) {
        close(zc[0]); close(rp[0]);
        int shm = recv_fd(zc[1]);              // memfd via SCM_RIGHTS
        if (shm < 0) _exit(50);
        pid_t r = fork();                       // zygote forks the "renderer"
        if (r == 0) {
            // shift the allocator so the shared page lands at a different VA than the browser's
            (void)mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            int *C = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
            if (C == MAP_FAILED) _exit(3);
            char b = 'R'; (void)!write(rp[1], &b, 1); // tell browser we are about to park
            struct timespec to = {3, 0};
            int woke = 0;
            for (;;) {
                if (atomic_load((_Atomic int *)C) != 0) { woke = 1; break; }
                long rc = fwait(C, 0, &to);
                if (rc == 0) { woke = 1; break; }
                if (rc == -1 && errno == ETIMEDOUT) { woke = 0; break; }
            }
            _exit(woke ? 0 : 1);
        }
        int st; waitpid(r, &st, 0);
        _exit(WIFEXITED(st) ? WEXITSTATUS(st) : 99);
    }

    close(zc[1]); close(rp[1]);
    int shm = (int)syscall(SYS_memfd_create, "cmdbuf", 0u);
    if (shm < 0 || ftruncate(shm, 4096) != 0) { perror("memfd"); return 1; }
    int *P = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
    if (P == MAP_FAILED) { perror("mmap"); return 1; }
    P[0] = 0;
    if (send_fd(zc[0], shm) != 0) { perror("send_fd"); return 1; }

    char rb = 0;
    (void)!read(rp[0], &rb, 1);                 // wait until the renderer has mapped + is parking
    struct timespec nap = {0, 250 * 1000 * 1000};
    nanosleep(&nap, NULL);                        // let it actually park in FUTEX_WAIT
    atomic_store((_Atomic int *)&P[0], 1);
    fwake(&P[0], INT_MAX);                         // wake through the browser's independent VA

    int zst = 0;
    waitpid(zyg, &zst, 0);
    int woke = (WIFEXITED(zst) && WEXITSTATUS(zst) == 0) ? 1 : 0;
    printf("scm_futex xproc_woke=%d\n", woke);
    fflush(stdout);
    return 0;
}
