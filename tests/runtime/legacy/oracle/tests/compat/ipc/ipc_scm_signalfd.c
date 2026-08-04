// SCM_RIGHTS passes a LIVE signalfd across an AF_UNIX socketpair; the receiver reads the pending signal
// off the received fd. signalfd is an engine-emulated object (per-fd self-pipe slot keyed by fd number);
// the received fd carries none of that routing state, so its read does not surface the signal
// (excluded-known-bug: emulated-fd identity not transferred across SCM_RIGHTS -- same root class as
// ipc_scm_timerfd, which IS fixed; signalfd's slot/refcount backing differs, deferred).
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/signalfd.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <poll.h>
int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    sigset_t m;
    sigemptyset(&m);
    sigaddset(&m, SIGUSR1);
    sigprocmask(SIG_BLOCK, &m, NULL);
    int sfd = signalfd(-1, &m, 0);
    if (sfd < 0) return 1;

    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    char data = 'S';
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
    memcpy(CMSG_DATA(c), &sfd, sizeof(int));
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
    int rsfd = -1;
    if (rc && rc->cmsg_type == SCM_RIGHTS) memcpy(&rsfd, CMSG_DATA(rc), sizeof(int));

    raise(SIGUSR1);
    struct pollfd pf = {rsfd, POLLIN, 0};
    int pr = poll(&pf, 1, 2000);
    struct signalfd_siginfo si;
    ssize_t n = (rsfd >= 0) ? read(rsfd, &si, sizeof si) : -1;
    int signo = (n == (ssize_t)sizeof si) ? (int)si.ssi_signo : -1;
    printf("poll=%d full_read=%d signo=%d\n", pr, n == (ssize_t)sizeof si, signo); // 1 1 10
    return 0;
}
