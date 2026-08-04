// EDGE-TRIGGERED pre-armed inbound: the parent writes the socket message + signals the eventfd BEFORE
// the child registers its epoll. The child then registers EPOLLIN|EPOLLET and epoll_waits with a SHORT
// timeout. On Linux the EPOLLET registration edge reports an already-readable fd (this is how multi-process application's
// child learns of a bootstrap message the coordinator sent before the child's message loop started). The
// engine emulates that with a per-process g_ep_prime probe (poll() at registration) -- this test proves
// whether that prime fires CROSS-PROCESS for data another process buffered. A short timeout means a lost
// prime shows as a clean FAIL (timeout), never a hang.
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

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) { perror("socketpair"); return 1; }
    int efd = eventfd(0, EFD_NONBLOCK);
    if (efd < 0) { perror("eventfd"); return 1; }
    int sync[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sync) != 0) { perror("sync"); return 1; }

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }
    if (pid == 0) {
        close(sv[0]);
        close(sync[0]);
        int chan = sv[1];
        // signal parent: "I exist, not yet registered -- write your inbound NOW"
        char g = 'g';
        if (write(sync[1], &g, 1) != 1) _exit(30);
        // wait until the parent says it has written (so the data is buffered BEFORE we register)
        char ack;
        if (read(sync[1], &ack, 1) != 1) _exit(31); // parent write end kept open; blocks until parent writes ack
        // NOW register edge-triggered, data already in the kernel buffers
        int ep = epoll_create1(EPOLL_CLOEXEC);
        struct epoll_event ce = {.events = EPOLLIN | EPOLLET, .data.fd = chan};
        struct epoll_event ee = {.events = EPOLLIN | EPOLLET, .data.fd = efd};
        epoll_ctl(ep, EPOLL_CTL_ADD, chan, &ce);
        epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ee);
        int gs = 0, ge = 0;
        char msg[64] = {0};
        uint64_t v = 0;
        struct epoll_event out[2];
        int n = epoll_wait(ep, out, 2, 1000); // short: a lost prime => timeout, not hang
        for (int i = 0; i < n; i++) {
            if (out[i].data.fd == chan) { ssize_t r = read(chan, msg, sizeof msg - 1); if (r > 0) { msg[r] = 0; gs = 1; } }
            else if (out[i].data.fd == efd) { if (read(efd, &v, 8) == 8) ge = 1; }
        }
        printf("child prearm n=%d sock=%d msg=%s efd=%d val=%llu\n", n, gs, msg, ge, (unsigned long long)v);
        fflush(stdout);
        _exit((gs && ge && v == 5) ? 0 : 42);
    }

    close(sv[1]);
    close(sync[1]);
    // wait for child's "I exist" signal
    char g;
    if (read(sync[0], &g, 1) != 1) { perror("sync read"); return 2; }
    // write inbound BEFORE the child registers epoll
    const char *m = "Invitation";
    if (write(sv[0], m, strlen(m)) < 0) { perror("write sock"); return 3; }
    uint64_t v = 5;
    if (write(efd, &v, 8) != 8) { perror("write efd"); return 4; }
    // tell the child the data is buffered; it may register now
    char ack = 'a';
    if (write(sync[0], &ack, 1) != 1) { perror("ack"); return 5; }

    int st = 0;
    waitpid(pid, &st, 0);
    printf("parent child_exit=%d\n", WIFEXITED(st) ? WEXITSTATUS(st) : -1);
    return 0;
}
