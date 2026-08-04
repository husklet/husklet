// epoll readiness + EEXIST/ENOENT semantics for a WATCHED FD WHOSE NUMBER EXCEEDS 1024.
// A large event loop registers hundreds of fds, so watched fd numbers routinely climb past
// 1024. The engine tracks per-epoll-instance membership in a bitmap indexed by the watched fd; the
// bitmap must span the full guarded fd range, not just the first 1024. If it is sized to only 1024
// bits, indexing it with fd>=1024 is a heap out-of-bounds access whose garbage membership bit either
// (a) spuriously returns EEXIST and drops the real EPOLL_CTL_ADD -> the fd is never armed -> its
// readiness is never delivered (a load-dependent node-connect stall), or (b) fails to
// record membership so a duplicate ADD is wrongly accepted. This reproduces both, deterministically,
// so the differential oracle catches any divergence from native Linux.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/resource.h>
#include <unistd.h>

int main(void) {
    // Permit high fd numbers (default soft NOFILE is often 1024, which would EMFILE at fd 1100).
    struct rlimit rl;
    if (getrlimit(RLIMIT_NOFILE, &rl) == 0) {
        rl.rlim_cur = rl.rlim_max; // raise soft to hard
        setrlimit(RLIMIT_NOFILE, &rl);
    }

    int fds[2];
    if (pipe(fds) < 0) { perror("pipe"); return 1; }

    // Relocate the pipe READ end to a fd number > 1024 (the regime past the 1024-bit bitmap).
    int hi = fcntl(fds[0], F_DUPFD, 1100);
    if (hi < 0) { perror("F_DUPFD"); return 1; }
    close(fds[0]);
    int hi_gt_1024 = hi > 1024;

    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = hi};

    // (1) ADD a >1024 watched fd. Must succeed (0).
    int add0 = epoll_ctl(ep, EPOLL_CTL_ADD, hi, &ev);

    // (2) Re-ADD the same >1024 fd. Linux returns EEXIST (membership must be recorded at this fd number).
    int add1 = epoll_ctl(ep, EPOLL_CTL_ADD, hi, &ev);
    int eexist = (add1 == -1 && errno == EEXIST);

    // (3) Readiness on the >1024 fd must be delivered (the lost-wakeup symptom).
    for (int i = 0; i < 5; i++) { ssize_t w = write(fds[1], "xyz", 3); (void)w; }
    close(fds[1]);
    long total = 0;
    int events_seen = 0;
    for (;;) {
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, 1000);
        if (n <= 0) break;
        events_seen++;
        char buf[64];
        ssize_t r = read(hi, buf, sizeof buf);
        if (r <= 0) break;
        total += r;
    }

    // (4) DEL then MOD the >1024 fd: DEL succeeds, a following MOD of the now-absent fd is ENOENT.
    int del0 = epoll_ctl(ep, EPOLL_CTL_DEL, hi, &ev);
    int mod1 = epoll_ctl(ep, EPOLL_CTL_MOD, hi, &ev);
    int enoent = (mod1 == -1 && errno == ENOENT);

    close(ep);
    close(hi);
    // Native Linux prints: hifd=1 add0=0 eexist=1 bytes=15 ev=1 del0=0 enoent=1
    printf("hifd=%d add0=%d eexist=%d bytes=%ld ev=%d del0=%d enoent=%d\n",
           hi_gt_1024, add0, eexist, total, events_seen >= 1, del0, enoent);
    return 0;
}
