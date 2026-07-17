#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define NFD 48

struct result {
    int recv_n;
    int trunc;
    int woke;
    int reads;
    unsigned long long sum;
};

static int send_fds(int sock, int *fds, int nfds) {
    char b = 'F';
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int) * NFD)];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int) * nfds);
    memcpy(CMSG_DATA(c), fds, sizeof(int) * nfds);
    mh.msg_controllen = c->cmsg_len;
    return sendmsg(sock, &mh, 0) == 1 ? 0 : -1;
}

static int recv_fds(int sock, int *fds, int maxfds, int *trunc) {
    char b = 0;
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int) * NFD)];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    if (recvmsg(sock, &mh, MSG_CMSG_CLOEXEC) != 1) return -1;
    if (trunc) *trunc = (mh.msg_flags & MSG_CTRUNC) != 0;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    if (!c || c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) return -1;
    int n = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
    if (n > maxfds) n = maxfds;
    memcpy(fds, CMSG_DATA(c), sizeof(int) * n);
    return n;
}

int main(void) {
    int sv[2], ready[2], out[2];
    int efd[NFD];
    memset(efd, -1, sizeof efd);
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) != 0 || pipe(ready) != 0 || pipe(out) != 0) {
        perror("setup");
        return 1;
    }
    for (int i = 0; i < NFD; i++) {
        efd[i] = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        if (efd[i] < 0) {
            perror("eventfd");
            return 1;
        }
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        close(sv[0]);
        close(ready[0]);
        close(out[0]);
        int got[NFD];
        memset(got, -1, sizeof got);
        struct result r = {0};
        r.recv_n = recv_fds(sv[1], got, NFD, &r.trunc);
        int ep = epoll_create1(EPOLL_CLOEXEC);
        if (ep >= 0 && r.recv_n == NFD) {
            for (int i = 0; i < NFD; i++) {
                struct epoll_event ev = {.events = EPOLLIN | EPOLLET, .data.u32 = (uint32_t)i};
                if (epoll_ctl(ep, EPOLL_CTL_ADD, got[i], &ev) != 0) _exit(2);
            }
            char b = 'R';
            if (write(ready[1], &b, 1) != 1) _exit(3);
            int seen[NFD] = {0};
            // Drain until every fd has woken, bounded by a generous iteration cap and a per-wait timeout.
            // A native/trusted run delivers all NFD readable eventfds in one or two big epoll_wait batches,
            // so a handful of loops sufficed. Under the untrusted sentry split EVERY syscall (the writer's
            // eventfd write() and this reader's read()) is a synchronous ring round-trip to the sentry, so
            // producer and consumer run lock-step at ~1 event per epoll_wait -- the batch never piles up and
            // the old 8-iteration cap abandoned the drain with ~40 eventfds still pending-but-readable (NOT
            // lost: their counters and readable edges survived the SCM_RIGHTS transfer intact). Loop up to
            // NFD*4 times so the reader can drain the trickle to completion. A GENUINELY lost wake is still
            // caught: once the writer is done and nothing new is readable, epoll_wait hits its 2000ms budget
            // and returns 0, breaking the loop below with woke < NFD -> a failing verdict.
            for (int loops = 0; loops < NFD * 4 && r.woke < NFD; loops++) {
                struct epoll_event evs[NFD];
                int n = epoll_wait(ep, evs, NFD, 2000);
                if (n <= 0) break;
                for (int j = 0; j < n; j++) {
                    int idx = (int)evs[j].data.u32;
                    if (idx < 0 || idx >= NFD || seen[idx]) continue;
                    uint64_t v = 0;
                    int rr = (int)read(got[idx], &v, sizeof v);
                    seen[idx] = 1;
                    r.woke++;
                    if (rr == 8) {
                        r.reads++;
                        r.sum += (unsigned long long)v;
                    }
                }
            }
        }
        for (int i = 0; i < NFD; i++)
            if (got[i] >= 0) close(got[i]);
        (void)write(out[1], &r, sizeof r);
        _exit(r.recv_n == NFD && !r.trunc && r.woke == NFD && r.reads == NFD &&
                      r.sum == (unsigned long long)NFD * (NFD + 1) / 2
                  ? 0
                  : 4);
    }

    close(sv[1]);
    close(ready[1]);
    close(out[1]);
    if (send_fds(sv[0], efd, NFD) != 0) {
        perror("send_fds");
        return 1;
    }
    char rb = 0;
    if (read(ready[0], &rb, 1) != 1) {
        struct result r = {0};
        ssize_t rn = read(out[0], &r, sizeof r);
        int st = 0;
        waitpid(pid, &st, 0);
        printf("scm_eventfd_dense recv=%d trunc=%d woke=%d read=%d sum=%llu child=%d\n", r.recv_n, r.trunc,
               r.woke, r.reads, r.sum, WIFEXITED(st) ? WEXITSTATUS(st) : -1);
        return rn == (ssize_t)sizeof r ? 1 : 2;
    }
    for (int i = 0; i < NFD; i++) {
        uint64_t v = (uint64_t)(i + 1);
        if (write(efd[i], &v, sizeof v) != (ssize_t)sizeof v) {
            perror("eventfd write");
            return 1;
        }
    }

    struct result r = {0};
    ssize_t rn = read(out[0], &r, sizeof r);
    int st = 0;
    waitpid(pid, &st, 0);
    for (int i = 0; i < NFD; i++) close(efd[i]);
    printf("scm_eventfd_dense recv=%d trunc=%d woke=%d read=%d sum=%llu child=%d\n", r.recv_n, r.trunc,
           r.woke, r.reads, r.sum, WIFEXITED(st) ? WEXITSTATUS(st) : -1);
    return rn == (ssize_t)sizeof r && r.recv_n == NFD && !r.trunc && r.woke == NFD && r.reads == NFD &&
                   r.sum == (unsigned long long)NFD * (NFD + 1) / 2 && WIFEXITED(st) && WEXITSTATUS(st) == 0
               ? 0
               : 1;
}
