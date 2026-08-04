// SCM_RIGHTS passes a LIVE timerfd across an AF_UNIX socketpair; the received fd must be a working dup
// of the sender's timer -- the receiver polls it ready and reads the accumulated expiration count.
// A timerfd is an engine-emulated object (host kqueue + per-fd deadline bookkeeping keyed by fd number);
// the received fd carries none of that state unless the ancillary path restores it like dup() does.
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/timerfd.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <stdint.h>
#include <poll.h>
int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    if (tfd < 0) return 1;
    struct itimerspec its = {{0, 0}, {0, 20 * 1000 * 1000}}; // 20ms one-shot
    if (timerfd_settime(tfd, 0, &its, NULL)) return 1;

    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    char data = 'T';
    struct iovec io = {&data, 1};
    struct msghdr mh = {0};
    mh.msg_iov = &io;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &tfd, sizeof(int));
    sendmsg(sv[1], &mh, 0);

    char rdata;
    struct iovec rio = {&rdata, 1};
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof rcbuf);
    struct msghdr rmh = {0};
    rmh.msg_iov = &rio;
    rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof rcbuf;
    recvmsg(sv[0], &rmh, 0);
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    int newfd = -1;
    int is_rights = rc && rc->cmsg_type == SCM_RIGHTS;
    if (is_rights) memcpy(&newfd, CMSG_DATA(rc), sizeof(int));

    struct pollfd pf = {newfd, POLLIN, 0};
    int pr = poll(&pf, 1, 2000);
    uint64_t expirations = 0;
    ssize_t n = (newfd >= 0) ? read(newfd, &expirations, 8) : -1;
    printf("scm_rights=%d distinct_fd=%d poll_ready=%d bytes=%zd expirations=%llu\n",
           is_rights, newfd != tfd, pr == 1 && (pf.revents & POLLIN) != 0, n,
           (unsigned long long)expirations); // 1 1 1 8 1
    return 0;
}
