// Cross-process INBOUND event delivery to a CHILD parked in epoll_wait -- the exact Chrome
// renderer/GPU Mojo scenario (child blocks on its transport fds; the parent/browser writes).
// The existing scm-eventfd gate tests the OPPOSITE direction (parent epolls, child writes), so this
// isolates whether a parent's socketpair write / eventfd signal wakes a child's epoll cross-process.
//
// Mode A (argv[1]=="child"): re-exec'd child. It inherits sv[1] on fd 3 and efd on fd 4 (non-CLOEXEC,
// placed by the parent), builds an epoll set over both, and blocks in epoll_wait; reports what woke it.
// Default: parent creates the transport, forks+execs the child, waits, then WRITES inbound.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// fixed child fd numbers (mirrors Chrome's base::LaunchOptions fd remapping)
#define CH_SOCK 3
#define CH_EFD  4

static int child_main(void) {
    int sock = CH_SOCK, efd = CH_EFD;
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event se = {.events = EPOLLIN, .data.fd = sock};
    struct epoll_event ee = {.events = EPOLLIN, .data.fd = efd};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, sock, &se) != 0) { printf("child epoll_ctl sock: %s\n", strerror(errno)); return 21; }
    if (epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ee) != 0) { printf("child epoll_ctl efd: %s\n", strerror(errno)); return 22; }
    // tell the parent we are parked and about to wait
    char rdy = 'R';
    if (write(sock, &rdy, 1) != 1) { printf("child ready-write: %s\n", strerror(errno)); return 23; }
    int got_sock = 0, got_efd = 0;
    char msg[64]; msg[0] = 0;
    uint64_t ev = 0;
    for (int rounds = 0; rounds < 2 && !(got_sock && got_efd); rounds++) {
        struct epoll_event out[2];
        int n = epoll_wait(ep, out, 2, 3000);
        if (n <= 0) { printf("child epoll_wait timeout n=%d round=%d got_sock=%d got_efd=%d\n", n, rounds, got_sock, got_efd); return 24; }
        for (int i = 0; i < n; i++) {
            if (out[i].data.fd == sock) {
                ssize_t r = read(sock, msg, sizeof msg - 1);
                if (r > 0) { msg[r] = 0; got_sock = 1; }
            } else if (out[i].data.fd == efd) {
                if (read(efd, &ev, 8) == 8) got_efd = 1;
            }
        }
    }
    printf("child woke sock=%d msg=%s efd=%d val=%llu\n", got_sock, msg, got_efd, (unsigned long long)ev);
    return (got_sock && got_efd && ev == 7) ? 0 : 25;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) return child_main();

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) { perror("socketpair"); return 1; }
    int efd = eventfd(0, 0);
    if (efd < 0) { perror("eventfd"); return 1; }

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }
    if (pid == 0) {
        // place child's transport at fixed fds, non-CLOEXEC, then re-exec self as "child"
        close(sv[0]);
        dup2(sv[1], CH_SOCK);
        dup2(efd, CH_EFD);
        fcntl(CH_SOCK, F_SETFD, 0);
        fcntl(CH_EFD, F_SETFD, 0);
        char self[512];
        ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
        if (sl <= 0) { printf("child readlink: %s\n", strerror(errno)); _exit(30); }
        self[sl] = 0;
        char *av[] = {self, (char *)"child", NULL};
        execv(self, av);
        printf("child execv: %s\n", strerror(errno));
        _exit(31);
    }

    close(sv[1]);
    // wait for the child's "R" readiness byte so our writes are genuinely INBOUND to a parked epoll
    char r;
    if (read(sv[0], &r, 1) != 1) { printf("parent ready-read: %s\n", strerror(errno)); return 2; }
    // brief settle so the child is inside epoll_wait
    struct timespec ts = {0, 50 * 1000 * 1000};
    nanosleep(&ts, NULL);
    // INBOUND: socketpair message + eventfd signal, browser->renderer direction
    const char *m = "BeginFrame";
    if (write(sv[0], m, strlen(m)) < 0) { perror("parent write sock"); return 3; }
    uint64_t v = 7;
    if (write(efd, &v, 8) != 8) { perror("parent write efd"); return 4; }

    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("parent done child_exit=%d\n", code);
    return code == 0 ? 0 : 1;
}
