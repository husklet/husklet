// Faithful Chrome renderer-launch repro: a fd is passed via SCM_RIGHTS to a pre-existing ZYGOTE
// process, which then FORKS the renderer child that inherits it. The ORIGINAL creator (browser)
// writes inbound. This is the exact path renderers take (browser -> zygote SCM_RIGHTS -> fork child),
// which differs from a plain fork-inherit (the GPU process path, already known to work).
//
// browser: creates channel socketpair(sv) + an eventfd(efd); sends sv[1]+efd to the zygote via
//   SCM_RIGHTS; then writes "BeginFrame" to sv[0] and signals efd. Reads the renderer's verdict off sv[0].
// zygote:  recvmsg's the two fds, forks a renderer that epoll_waits on both and reports.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int send_two_fds(int sock, int a, int b) {
    char x = 'x';
    struct iovec io = {.iov_base = &x, .iov_len = 1};
    char ctl[CMSG_SPACE(2 * sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(2 * sizeof(int));
    int fds[2] = {a, b};
    memcpy(CMSG_DATA(c), fds, sizeof fds);
    return sendmsg(sock, &mh, 0) == 1 ? 0 : -1;
}

static int recv_two_fds(int sock, int *a, int *b) {
    char x;
    struct iovec io = {.iov_base = &x, .iov_len = 1};
    char ctl[CMSG_SPACE(2 * sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    if (recvmsg(sock, &mh, 0) != 1) return -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    if (!c || c->cmsg_type != SCM_RIGHTS) return -1;
    int fds[2];
    memcpy(fds, CMSG_DATA(c), sizeof fds);
    *a = fds[0];
    *b = fds[1];
    return 0;
}

// renderer: epoll on the inherited channel + eventfd, report verdict on the channel.
static void renderer_main(int chan, int efd) {
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ce = {.events = EPOLLIN, .data.fd = chan};
    struct epoll_event ee = {.events = EPOLLIN, .data.fd = efd};
    epoll_ctl(ep, EPOLL_CTL_ADD, chan, &ce);
    epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ee);
    char rdy = 'R';
    if (write(chan, &rdy, 1) != 1) _exit(40); // tell browser we are parked
    int gs = 0, ge = 0;
    char msg[64] = {0};
    uint64_t v = 0;
    for (int i = 0; i < 4 && !(gs && ge); i++) {
        struct epoll_event out[2];
        int n = epoll_wait(ep, out, 2, 3000);
        if (n <= 0) break;
        for (int j = 0; j < n; j++) {
            if (out[j].data.fd == chan) {
                ssize_t r = read(chan, msg, sizeof msg - 1);
                if (r > 0) { msg[r] = 0; gs = 1; }
            } else if (out[j].data.fd == efd) {
                if (read(efd, &v, 8) == 8) ge = 1;
            }
        }
    }
    char verdict[80];
    int k = snprintf(verdict, sizeof verdict, "V sock=%d msg=%s efd=%d val=%llu", gs, msg, ge, (unsigned long long)v);
    if (write(chan, verdict, k) < 0) _exit(41);
    _exit((gs && ge && v == 7) ? 0 : 42);
}

int main(void) {
    // control channel browser<->zygote
    int zc[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, zc) != 0) { perror("zc"); return 1; }
    pid_t zyg = fork();
    if (zyg < 0) { perror("fork zygote"); return 1; }
    if (zyg == 0) {
        close(zc[0]);
        int chan = -1, efd = -1;
        if (recv_two_fds(zc[1], &chan, &efd) != 0) _exit(50);
        pid_t r = fork(); // zygote forks the renderer
        if (r == 0) { close(zc[1]); renderer_main(chan, efd); }
        // zygote: reap renderer, then exit
        int st;
        waitpid(r, &st, 0);
        _exit(0);
    }

    close(zc[1]);
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) { perror("sv"); return 1; }
    int efd = eventfd(0, 0);
    if (efd < 0) { perror("eventfd"); return 1; }
    if (send_two_fds(zc[0], sv[1], efd) != 0) { perror("send_two_fds"); return 1; }
    close(sv[1]);

    // wait for the renderer's readiness byte (it is a grandchild; the channel is our only link)
    char r;
    if (read(sv[0], &r, 1) != 1) { printf("browser ready-read: %s\n", strerror(errno)); return 2; }
    struct timespec ts = {0, 50 * 1000 * 1000};
    nanosleep(&ts, NULL);
    const char *m = "BeginFrame";
    if (write(sv[0], m, strlen(m)) < 0) { perror("write chan"); return 3; }
    uint64_t v = 7;
    if (write(efd, &v, 8) != 8) { perror("write efd"); return 4; }

    char verdict[128] = {0};
    ssize_t vn = read(sv[0], verdict, sizeof verdict - 1);
    if (vn > 0) verdict[vn] = 0;
    int zst = 0;
    waitpid(zyg, &zst, 0);
    printf("browser got %s\n", vn > 0 ? verdict : "(no verdict)");
    return 0;
}
