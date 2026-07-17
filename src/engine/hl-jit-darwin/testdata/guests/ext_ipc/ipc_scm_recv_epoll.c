// SCM_RIGHTS-RECEIVED socket armed on the CHILD's own epoll, woken by a cross-process write to the
// parent's RETAINED peer end -- the exact Mojo AcceptBrokerClient/AcceptInvitee step. A node-channel
// control message carries an out-of-band platform SOCKET handle; the child recvmsg's that fd and
// installs the RECEIVED fd (a fresh guest/host fd number) on its kqueue/epoll, then parks in
// epoll_wait until the peer writes.
//
// This is the one permutation no existing gate covers:
//   - scm-eventfd  epolls a fd the PARENT created (and it is an eventfd, not a socket).
//   - zygote-inbound epolls a FORK-INHERITED socket: the receiver recvmsg's it, then fork()s a child
//     that epoll_ctl's the INHERITED fd -- kqueue arming rides fork inheritance, not the recvmsg fd.
//   - scm-futex delivers an SCM_RIGHTS memfd but waits on FUTEX, never epoll/kqueue.
// Here the SAME process that recvmsg's the socket is the one that arms it on epoll -- so if hl fails to
// arm the host kqueue EVFILT_READ on an fd installed by recvmsg's SCM_RIGHTS path (net.c:1283 ->
// cmsg_m2l), the child never gets EPOLLIN and parks forever. A watchdog thread turns that lost wake
// into a deterministic nonzero exit (never a hang). ROUNDS repeats the drain->re-block cycle so a
// readiness edge lost specifically on RE-ARM of a received socket is also caught.
//
// Layout (Chrome-faithful): parent = browser; child = fork+execve renderer.
//   node[2]  : primordial node channel (SEQPACKET). child end dup2'd to fd 3 at launch (inherited).
//   broker[2]: the out-of-band peer/broker channel (SOCK_STREAM, like Chrome's ty=1 node channel).
//   parent sends broker[1] to the child over the NODE channel via SCM_RIGHTS; retains broker[0].
//   child recvmsg's broker[1] off fd 3, epoll_ctl(ADD, EPOLLIN) the RECEIVED fd, and epoll_waits.
//   each round: parent writes to broker[0]; child must wake+read, then ACK over the node channel.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CH_NODE 3 // primordial node channel — child end, placed by the parent at a fixed fd
#define ROUNDS  64

static int send_fd(int sock, int fd) {
    char b = 'x';
    struct iovec io = {.iov_base = &b, .iov_len = 1};
    char ctl[CMSG_SPACE(sizeof(int))];
    memset(ctl, 0, sizeof ctl);
    struct msghdr mh = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
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
    if (!c || c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) return -1;
    int fd = -1;
    memcpy(&fd, CMSG_DATA(c), sizeof fd);
    return fd;
}

static _Atomic long g_rounds = 0;

static void *child_watchdog(void *arg) {
    (void)arg;
    long last = -1;
    for (;;) {
        struct timespec ts = {0, 400 * 1000 * 1000};
        nanosleep(&ts, NULL);
        long s = atomic_load(&g_rounds);
        if (s >= ROUNDS) return NULL;
        if (s == last) {
            fprintf(stderr, "STALL rounds=%ld/%d (received-socket epoll never woke)\n", s, ROUNDS);
            fflush(stderr);
            _exit(7); // lost wake: child parked in epoll_wait with a message pending on the received fd
        }
        last = s;
    }
}

static int child_main(void) {
    int node = CH_NODE;
    // AcceptBrokerClient: pull the out-of-band peer socket off the node channel via SCM_RIGHTS.
    int broker = recv_fd(node);
    if (broker < 0) {
        printf("child recv_fd failed: %s\n", strerror(errno));
        return 20;
    }
    // Edge-triggered: drain to EAGAIN each round so a readiness edge LOST on RE-ARM of the received
    // socket (the exact "never re-armed on the child's kqueue" hypothesis) parks the child -> watchdog.
    fcntl(broker, F_SETFL, fcntl(broker, F_GETFL) | O_NONBLOCK);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    if (ep < 0) return 21;
    // Arm the RECEIVED socket on our own epoll. A lost arm/edge => epoll_wait times out (never a hang).
    struct epoll_event be = {.events = EPOLLIN | EPOLLET, .data.fd = broker};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, broker, &be) != 0) {
        printf("child epoll_ctl(received broker): %s\n", strerror(errno));
        return 22;
    }
    pthread_t wd;
    pthread_create(&wd, NULL, child_watchdog, NULL);

    // Tell the parent we have received + armed the fd and are about to park (writes are genuinely INBOUND).
    char rdy = 'R';
    if (write(node, &rdy, 1) != 1) return 23;

    char msg[64];
    for (long i = 0; i < ROUNDS; i++) {
        struct epoll_event out[1];
        int n = epoll_wait(ep, out, 1, 4000);
        if (n <= 0) {
            fprintf(stderr, "child epoll_wait timeout n=%d round=%ld\n", n, i);
            return 24; // a lost readiness edge on the received socket
        }
        if (out[0].data.fd != broker) return 25;
        // edge-triggered: drain to EAGAIN so the NEXT message must produce a fresh re-arm edge
        int got = 0;
        for (;;) {
            ssize_t r = read(broker, msg, sizeof msg - 1);
            if (r > 0) {
                got = 1;
                continue;
            }
            if (r < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) break;
            if (r == 0) return 26; // peer closed unexpectedly
            if (r < 0) return 29;
        }
        if (!got) return 30;
        atomic_fetch_add(&g_rounds, 1);
        // ACK over the node channel; the parent waits for this before planting the NEXT message, so each
        // write lands after we have drained + are re-blocking -> exercises RE-ARM of the received fd.
        char a = 'A';
        if (write(node, &a, 1) != 1) return 27;
    }
    printf("child recv-epoll rounds=%ld/%d ok=%d\n", atomic_load(&g_rounds), ROUNDS,
           atomic_load(&g_rounds) == ROUNDS);
    return atomic_load(&g_rounds) == ROUNDS ? 0 : 28;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) return child_main();

    int node[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, node) != 0) {
        perror("socketpair node");
        return 1;
    }
    int broker[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, broker) != 0) {
        perror("socketpair broker");
        return 1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        // Child inherits neither broker end directly — it must RECEIVE broker[1] via SCM_RIGHTS.
        close(node[0]);
        close(broker[0]);
        close(broker[1]);
        dup2(node[1], CH_NODE);
        fcntl(CH_NODE, F_SETFD, 0);
        char self[512];
        ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
        if (sl <= 0) _exit(30);
        self[sl] = 0;
        char *av[] = {self, (char *)"child", NULL};
        execv(self, av);
        _exit(31);
    }

    close(node[1]);
    // parent retains node[0] (handshake) and broker[0] (the peer we write to wake the child).
    if (send_fd(node[0], broker[1]) != 0) {
        perror("send_fd broker");
        return 2;
    }
    close(broker[1]); // the child owns the passed reference now

    // Wait for the child's readiness byte: it has received + armed the fd and is parking.
    char r;
    if (read(node[0], &r, 1) != 1) {
        printf("parent ready-read: %s\n", strerror(errno));
        return 3;
    }

    long sent = 0;
    for (long i = 0; i < ROUNDS; i++) {
        // Ensure the child is genuinely parked in epoll_wait before we write (inbound to a dormant peer).
        struct timespec settle = {0, 3 * 1000 * 1000};
        nanosleep(&settle, NULL);
        const char *m = "BeginFrame";
        if (write(broker[0], m, strlen(m)) < 0) {
            perror("parent write broker");
            return 4;
        }
        sent++;
        char a;
        if (read(node[0], &a, 1) != 1) { // child ACKs after draining + re-blocking
            printf("parent ack-read round=%ld: %s\n", i, strerror(errno));
            return 5;
        }
    }

    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("parent done sent=%ld child_exit=%d\n", sent, code);
    return code == 0 ? 0 : 1;
}
