// Cross-process EDGE-TRIGGERED (EPOLLET) SEQPACKET streaming with each message planted in the child's
// drain->re-arm window -- the exact "readiness edge lost between drain and re-block" pattern, sustained
// (the existing xproc-inbound/xproc-prearm gates cover only a single-shot delivery). Models multi-process application's
// worker IO pump: the child epolls its IPC channel (SEQPACKET, EPOLLET) + a ScheduleWork wakeup
// eventfd, drains to EAGAIN, then signals the parent "drained" over a control fd; the parent immediately
// sends the NEXT large (>2KB, invitation-sized) message so it lands right after the drain, at re-arm.
// A lost edge => the child parks forever => the watchdog turns the stall into a deterministic nonzero
// exit. All N messages must arrive in order (also catches datagram drop/reorder on the DGRAM backing).
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
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CH_SOCK 3 // IPC channel (SEQPACKET) — child end
#define CH_EFD  4 // ScheduleWork wakeup eventfd
#define CH_CTL  5 // control back-channel: child -> parent "drained" tokens
#define ROUNDS  4000
#define MSGSZ   8192 // > 2KB invitation-sized

static _Atomic long g_seq = 0;   // next expected seq the child wants
static _Atomic int g_fail = 0;

static void *child_watchdog(void *arg) {
    (void)arg;
    long last = -1;
    for (;;) {
        struct timespec ts = {0, 300 * 1000 * 1000};
        nanosleep(&ts, NULL);
        long s = atomic_load(&g_seq);
        if (s >= ROUNDS) return NULL;
        if (s == last) {
            fprintf(stderr, "STALL seq=%ld/%d\n", s, ROUNDS);
            fflush(stderr);
            _exit(7); // lost wakeup: child parked with a message pending
        }
        last = s;
    }
}

static int child_main(void) {
    int sock = CH_SOCK, efd = CH_EFD, ctl = CH_CTL;
    // channel + eventfd non-blocking for edge-triggered drain
    fcntl(sock, F_SETFL, fcntl(sock, F_GETFL) | O_NONBLOCK);
    fcntl(efd, F_SETFL, fcntl(efd, F_GETFL) | O_NONBLOCK);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event se = {.events = EPOLLIN | EPOLLET, .data.fd = sock};
    struct epoll_event ee = {.events = EPOLLIN | EPOLLET, .data.fd = efd};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, sock, &se) != 0) return 21;
    if (epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ee) != 0) return 22;

    pthread_t wd;
    pthread_create(&wd, NULL, child_watchdog, NULL);

    // signal parent we are parked; parent then sends msg seq 0
    char rdy = 'R';
    if (write(ctl, &rdy, 1) != 1) return 23;

    static char buf[MSGSZ + 64];
    while (atomic_load(&g_seq) < ROUNDS) {
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, 4000);
        if (n <= 0) { atomic_store(&g_fail, 1); return 24; } // timeout: a lost edge
        for (int i = 0; i < n; i++) {
            if (out[i].data.fd == efd) {
                uint64_t v;
                while (read(efd, &v, 8) == 8) {}
            } else if (out[i].data.fd == sock) {
                // edge-triggered: drain to EAGAIN
                for (;;) {
                    ssize_t r = recv(sock, buf, sizeof buf, 0);
                    if (r <= 0) break;
                    long got;
                    memcpy(&got, buf, 8);
                    long want = atomic_load(&g_seq);
                    if (got != want) { fprintf(stderr, "ORDER got=%ld want=%ld\n", got, want); atomic_store(&g_fail, 1); return 25; }
                    atomic_fetch_add(&g_seq, 1);
                }
                // drained: tell the parent to plant the next message in our re-arm window
                if (atomic_load(&g_seq) < ROUNDS) {
                    char d = 'D';
                    if (write(ctl, &d, 1) != 1) return 26;
                }
            }
        }
    }
    printf("child stream got=%ld/%d ok=%d\n", atomic_load(&g_seq), ROUNDS, atomic_load(&g_seq) == ROUNDS && !atomic_load(&g_fail));
    return (atomic_load(&g_seq) == ROUNDS && !atomic_load(&g_fail)) ? 0 : 27;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) return child_main();

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) { perror("socketpair"); return 1; }
    int efd = eventfd(0, EFD_NONBLOCK);
    int ctl[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, ctl) != 0) { perror("ctl"); return 1; }
    if (efd < 0) { perror("eventfd"); return 1; }

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }
    if (pid == 0) {
        close(sv[0]); close(ctl[0]);
        dup2(sv[1], CH_SOCK); dup2(efd, CH_EFD); dup2(ctl[1], CH_CTL);
        fcntl(CH_SOCK, F_SETFD, 0); fcntl(CH_EFD, F_SETFD, 0); fcntl(CH_CTL, F_SETFD, 0);
        char self[512];
        ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
        if (sl <= 0) _exit(30);
        self[sl] = 0;
        char *av[] = {self, (char *)"child", NULL};
        execv(self, av);
        _exit(31);
    }
    close(sv[1]); close(ctl[1]);

    // wait for child "R"
    char r;
    if (read(ctl[0], &r, 1) != 1) return 2;

    static char msg[MSGSZ];
    memset(msg, 0xAB, sizeof msg);
    long sent = 0;
    // send seq 0 to kick off
    memcpy(msg, &sent, 8);
    if (send(sv[0], msg, sizeof msg, 0) < 0) { perror("send0"); return 3; }
    sent++;
    // occasionally also ScheduleWork the eventfd
    // then: each time the child says "drained", plant the next message in its re-arm window
    while (sent < ROUNDS) {
        char d;
        ssize_t cr = read(ctl[0], &d, 1);
        if (cr != 1) { fprintf(stderr, "parent ctl read cr=%zd sent=%ld\n", cr, sent); return 4; }
        memcpy(msg, &sent, 8);
        if (send(sv[0], msg, sizeof msg, 0) < 0) { perror("send"); return 5; }
        if ((sent & 3) == 0) { uint64_t v = 1; if (write(efd, &v, 8) != 8) {} }
        sent++;
    }
    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("parent done sent=%ld child_exit=%d\n", sent, code);
    return code;
}
