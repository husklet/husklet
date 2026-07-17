// Renderer Mojo PRIMARY-CHANNEL bring-up under MessagePumpEpoll, faithfully modeled + maximally loaded.
//
// WHY THIS GATE EXISTS. Multi-process Chrome content is white because the RENDERER never rasters: its
// primordial Mojo channel / IO-thread pump never completes bring-up, so it sits dormant (parked in
// epoll_wait + FUTEX_WAIT) — see docs/rendering/{README.md §3.2, CHROME-TILE-B1-B2-RESOLVED.md,
// WL_SHM-MULTIPROC-CONTENT-FINDINGS.md}. Every EXISTING cross-process micro-gate PASSES
// (scm-recv-epoll, exec-fd-epoll, xproc-inbound, zygote-inbound, scm-futex, pump-worker-dispatch,
// pump-xproc-et/oneshot, epoll-shared-xthread, pump-epollout-rearm), and a live trace once saw the
// renderer connect + pump hundreds of messages — so the wall is a LOAD-SENSITIVE / timing race, not a
// missing primitive. This gate reproduces the exact permutation those gates DON'T combine.
//
// WHAT NO OTHER GATE COMBINES (each existing gate isolates ONE leg):
//   * scm-recv-epoll:  SCM_RIGHTS-received SOCK_STREAM + execve, but EPOLLET, ONE watched fd, single
//                      thread, FIXED settle delay. This gate is LEVEL-triggered (EPOLLIN, no EPOLLET) —
//                      hl's kqueue must synthesize level re-report — under a large interest set.
//   * epoll-shared-xthread: concurrent cross-thread epoll_ctl on a shared epoll, but no received socket,
//                      no exec, no two-pump handoff.
//   * pump-worker-dispatch: EPOLLET IO pump + eventfd ScheduleWork + FUTEX handoff, but in-process
//                      (socketpair, no exec, no SCM_RIGHTS-received channel), ONE watched socket.
// Here ALL of it is one process shape, matching base::MessagePumpEpoll's renderer IO thread:
//   (a) the Mojo PRIMARY channel is an SCM_RIGHTS-RECEIVED SOCK_STREAM the child recvmsg's AFTER execve
//       (state reset by exec, fd is a fresh number installed via the recvmsg SCM_RIGHTS path, net.c
//       cmsg_m2l) and installs on its OWN epoll;
//   (b) a LEVEL-triggered epoll pump (EPOLLIN, NOT EPOLLET) watches the primary channel + a ScheduleWork
//       eventfd, reading ONE message per readiness (MessagePumpEpoll processes one FdWatcher event then
//       relies on the kernel's LEVEL re-report for the rest — so every extra byte needs a fresh level
//       report, the path EPOLLET gates never touch);
//   (c) a HIGH-FD regime: >=1024 idle watched fds on the SAME epoll (real renderers watch hundreds–
//       thousands of fds) so the kqueue interest set / changelist is large;
//   (d) a CHURN thread hammers concurrent epoll_ctl ADD/DEL on that same epoll while the pump is blocked
//       in epoll_wait — cross-thread changelist pressure on the live instance;
//   (e) the IO thread hands each decoded message to the MAIN thread via a cross-thread WaitableEvent
//       (eventfd), and the main thread runs its OWN MessagePumpEpoll (second epoll) — the IO->main
//       ScheduleWork wakeup none of the single-pump gates model;
//   (f) the browser writes each message after a VARIABLE delay (jittered), so messages land both while
//       the pump is fully parked AND inside the drain/re-block window.
// Assert: every message the browser sends is woken + recv'd on the IO pump AND dispatched + processed on
// the main pump within a bound. A lost readiness edge / lost cross-thread wake on ANY leg parks a pump
// with work pending = the dormant renderer; a watchdog turns that into a deterministic nonzero exit
// (exit 7), never a hang. If this reproduces the stall, localize in hl-jit-darwin
// (src/runtime/os/linux/syscall/event.c: kqueue readiness-prime / level re-report for an SCM_RIGHTS-
// received socket under concurrent epoll_ctl + high-fd load, or the eventfd cross-thread wake).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CH_NODE 3      // primordial node channel — child end, placed by the parent at a fixed fd (Mojo launch)
#define ROUNDS  12000  // messages the browser streams over the primary channel
#define NFILLER 3000   // idle watched fds on the SAME epoll — the high-fd regime (>=1024)
#define CHURN   96     // fds each churn thread cycles ADD/DEL on the live epoll
#define NCHURN  2      // concurrent churn threads (more changelist pressure on the live instance)
#define REARM   37     // every Nth message, StopWatch->Watch the primary (DEL+ADD) racing pending data
#define MSGSZ   8      // one primary-channel message = an 8-byte sequence number

static int g_primary;                 // SCM_RIGHTS-received SOCK_STREAM (the Mojo primary channel)
static int g_io_ep, g_main_ep;        // the IO-thread pump epoll + the main-thread pump epoll
static int g_schedwork_io;            // eventfd: ScheduleWork for the IO pump (posted work / self-wake)
static int g_waitable;                // eventfd: cross-thread WaitableEvent, IO thread -> main thread
static _Atomic long g_processed = 0;  // messages the MAIN pump has dispatched + processed
static _Atomic long g_recvd = 0;      // messages the IO pump has recv'd off the primary channel
static _Atomic int  g_done = 0;

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

// MAIN-thread MessagePumpEpoll: parks on its own epoll watching the cross-thread WaitableEvent (eventfd).
// The IO thread signals it per decoded message; here we "run the task": count it, and every 256 ACK the
// browser over the node channel for coarse backpressure. A lost IO->main wake parks this pump = dormant.
static void *main_pump(void *arg) {
    (void)arg;
    struct epoll_event out[8];
    long acked = 0;
    while (!atomic_load(&g_done)) {
        int n = epoll_wait(g_main_ep, out, 8, 500);
        if (n <= 0) continue; // timeout: watchdog owns liveness; loop re-checks g_done
        for (int i = 0; i < n; i++) {
            if (out[i].data.fd != g_waitable) continue;
            uint64_t v = 0;
            if (read(g_waitable, &v, 8) == 8 && v > 0) {
                long done = atomic_fetch_add(&g_processed, (long)v) + (long)v;
                // coarse cross-process backpressure ACK so the browser bounds outstanding messages
                while (done - acked >= 256) {
                    char a = 'A';
                    if (write(CH_NODE, &a, 1) != 1) return NULL;
                    acked += 256;
                }
                if (done >= ROUNDS) { atomic_store(&g_done, 1); return NULL; }
            }
        }
    }
    return NULL;
}

// CHURN thread: concurrent cross-thread epoll_ctl ADD/DEL on the LIVE io_ep while the IO pump is blocked
// in epoll_wait on it — changelist pressure on the same instance carrying the primary-channel watch.
static void *churn_thread(void *arg) {
    (void)arg;
    int pool[CHURN];
    for (int i = 0; i < CHURN; i++) pool[i] = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    int in[CHURN];
    memset(in, 0, sizeof in);
    unsigned r = 0x9e3779b9u;
    while (!atomic_load(&g_done)) {
        r = r * 1664525u + 1013904223u;
        int k = (int)(r % CHURN);
        if (pool[k] < 0) continue;
        if (!in[k]) {
            struct epoll_event ev = {.events = EPOLLIN, .data.fd = pool[k]}; // level-triggered, idle
            if (epoll_ctl(g_io_ep, EPOLL_CTL_ADD, pool[k], &ev) == 0) in[k] = 1;
        } else {
            if (epoll_ctl(g_io_ep, EPOLL_CTL_DEL, pool[k], NULL) == 0) in[k] = 0;
        }
        if ((r & 0x3f) == 0) { struct timespec ts = {0, 20 * 1000}; nanosleep(&ts, NULL); }
    }
    for (int i = 0; i < CHURN; i++) if (pool[i] >= 0) close(pool[i]);
    return NULL;
}

static void *watchdog(void *arg) {
    (void)arg;
    long last = -1;
    for (;;) {
        struct timespec ts = {0, 500 * 1000 * 1000};
        nanosleep(&ts, NULL);
        long p = atomic_load(&g_processed);
        if (p >= ROUNDS || atomic_load(&g_done)) return NULL;
        if (p == last) {
            fprintf(stderr, "STALL processed=%ld recvd=%ld/%d (renderer pump parked with work pending)\n",
                    p, atomic_load(&g_recvd), ROUNDS);
            fflush(stderr);
            _exit(7); // lost wake: a pump is dormant with a message pending — the renderer-dormancy shape
        }
        last = p;
    }
}

static int child_main(void) {
    int node = CH_NODE;
    // AcceptInvitee/AcceptBrokerClient: pull the Mojo PRIMARY channel (SOCK_STREAM) off the node channel.
    g_primary = recv_fd(node);
    if (g_primary < 0) { printf("child recv primary failed: %s\n", strerror(errno)); return 20; }
    fcntl(g_primary, F_SETFL, fcntl(g_primary, F_GETFL) | O_NONBLOCK);

    struct rlimit rl;
    if (getrlimit(RLIMIT_NOFILE, &rl) == 0) { rl.rlim_cur = rl.rlim_max; setrlimit(RLIMIT_NOFILE, &rl); }

    g_schedwork_io = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    g_waitable     = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (g_schedwork_io < 0 || g_waitable < 0) { printf("child eventfd: %s\n", strerror(errno)); return 21; }

    g_io_ep   = epoll_create1(EPOLL_CLOEXEC);
    g_main_ep = epoll_create1(EPOLL_CLOEXEC);
    if (g_io_ep < 0 || g_main_ep < 0) { printf("child epoll_create1: %s\n", strerror(errno)); return 22; }

    // IO pump watches: the primary channel + the ScheduleWork eventfd — LEVEL-triggered (NO EPOLLET).
    struct epoll_event pe = {.events = EPOLLIN, .data.fd = g_primary};
    if (epoll_ctl(g_io_ep, EPOLL_CTL_ADD, g_primary, &pe) != 0) { printf("child arm primary: %s\n", strerror(errno)); return 23; }
    struct epoll_event se = {.events = EPOLLIN, .data.fd = g_schedwork_io};
    epoll_ctl(g_io_ep, EPOLL_CTL_ADD, g_schedwork_io, &se);

    // High-fd regime: >=1024 idle watched fds on the SAME epoll (never become ready) — a large interest set.
    int nfill = 0;
    for (int i = 0; i < NFILLER; i++) {
        int fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
        if (fd < 0) break;
        struct epoll_event fe = {.events = EPOLLIN, .data.fd = fd};
        if (epoll_ctl(g_io_ep, EPOLL_CTL_ADD, fd, &fe) == 0) nfill++;
    }
    if (nfill < 1024) { printf("child high-fd regime not reached: nfill=%d (need >=1024)\n", nfill); return 24; }

    // MAIN pump watches the cross-thread WaitableEvent — level-triggered.
    struct epoll_event we = {.events = EPOLLIN, .data.fd = g_waitable};
    epoll_ctl(g_main_ep, EPOLL_CTL_ADD, g_waitable, &we);

    pthread_t mp, ch[NCHURN], wd;
    pthread_create(&mp, NULL, main_pump, NULL);
    for (int i = 0; i < NCHURN; i++) pthread_create(&ch[i], NULL, churn_thread, NULL);
    pthread_create(&wd, NULL, watchdog, NULL);

    // Tell the browser the primary channel is received + armed and both pumps are up.
    char rdy = 'R';
    if (write(node, &rdy, 1) != 1) return 25;

    // IO-thread MessagePumpEpoll: park on epoll_wait; on the primary channel's LEVEL readiness read ONE
    // message and rely on the kernel's level re-report for the rest, handing each to the main thread via
    // the WaitableEvent eventfd. This is the renderer's IO thread.
    char buf[MSGSZ * 8];
    while (!atomic_load(&g_done)) {
        struct epoll_event out[16];
        int n = epoll_wait(g_io_ep, out, 16, 4000);
        if (n < 0) { if (errno == EINTR) continue; return 26; }
        if (n == 0) {
            if (atomic_load(&g_recvd) >= ROUNDS || atomic_load(&g_done)) break;
            fprintf(stderr, "IO pump epoll_wait timeout recvd=%ld/%d\n", atomic_load(&g_recvd), ROUNDS);
            return 27; // parked 4s with no readiness though the browser is streaming = lost wake
        }
        for (int i = 0; i < n; i++) {
            if (out[i].data.fd == g_schedwork_io) {
                uint64_t v; while (read(g_schedwork_io, &v, 8) == 8) {}
            } else if (out[i].data.fd == g_primary) {
                // Read ONE syscall's worth (LEVEL re-report drives the remainder). Count whole messages.
                ssize_t r = read(g_primary, buf, sizeof buf);
                if (r == 0) { atomic_store(&g_done, 1); break; }
                if (r < 0) { if (errno == EAGAIN || errno == EWOULDBLOCK) continue; return 28; }
                long msgs = r / MSGSZ;
                if (msgs <= 0) msgs = 0;
                long total = atomic_fetch_add(&g_recvd, msgs) + msgs;
                // Cross-thread WaitableEvent signal (IO -> main): one unit per decoded message.
                uint64_t add = (uint64_t)msgs;
                if (add && write(g_waitable, &add, 8) != 8) return 29;
                // base::MessagePumpEpoll StopWatch -> Watch: periodically DE-register then RE-register the
                // RECEIVED primary channel. Because we read only ONE syscall's worth per wake (level pump)
                // and the browser bursts, the socket buffer is often NON-EMPTY at this point — so the re-ADD
                // of an already-readable SCM_RIGHTS-received socket MUST re-prime level readiness (the exact
                // "readiness never re-armed after the handle lands" hypothesis, now on RE-registration under
                // the high-fd + concurrent-ctl load). A lost prime parks the pump with data pending.
                if (total % REARM == 0) {
                    epoll_ctl(g_io_ep, EPOLL_CTL_DEL, g_primary, NULL);
                    struct epoll_event re = {.events = EPOLLIN, .data.fd = g_primary};
                    if (epoll_ctl(g_io_ep, EPOLL_CTL_ADD, g_primary, &re) != 0) return 31;
                }
            }
        }
    }
    // let the main pump finish dispatching everything it was handed
    for (int spins = 0; atomic_load(&g_processed) < ROUNDS && spins < 4000; spins++) {
        struct timespec ts = {0, 1000 * 1000}; nanosleep(&ts, NULL);
    }
    atomic_store(&g_done, 1);
    uint64_t one = 1; if (write(g_waitable, &one, 8) != 8) { /* wake main to exit */ }
    long p = atomic_load(&g_processed);
    printf("child primary-pump rounds=%ld/%d ok=%d\n", p, ROUNDS, p == ROUNDS);
    return p == ROUNDS ? 0 : 30;
}

int main(int argc, char **argv) {
    signal(SIGPIPE, SIG_IGN); // a stalled-child write must return EPIPE, not kill the browser
    if (argc > 1 && !strcmp(argv[1], "child")) return child_main();

    int node[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, node) != 0) { perror("socketpair node"); return 1; }
    int primary[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, primary) != 0) { perror("socketpair primary"); return 1; }

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }
    if (pid == 0) {
        close(node[0]);
        close(primary[0]);
        close(primary[1]); // the child must RECEIVE primary[1] via SCM_RIGHTS, not inherit it
        dup2(node[1], CH_NODE);
        fcntl(CH_NODE, F_SETFD, 0);
        char self[512];
        ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
        if (sl <= 0) _exit(40);
        self[sl] = 0;
        char *av[] = {self, (char *)"child", NULL};
        execv(self, av);
        _exit(41);
    }

    close(node[1]);
    if (send_fd(node[0], primary[1]) != 0) { perror("send_fd primary"); return 2; }
    close(primary[1]);

    char r;
    if (read(node[0], &r, 1) != 1) { printf("parent ready-read: %s\n", strerror(errno)); return 3; }

    // Browser: stream ROUNDS messages, each after a VARIABLE (jittered) delay so messages land both while
    // the pump is fully parked and inside its drain/re-block window. Bounded outstanding via the child's
    // periodic ACK bytes (read non-blocking on the node channel).
    fcntl(node[0], F_SETFL, fcntl(node[0], F_GETFL) | O_NONBLOCK);
    long sent = 0, acked = 0;
    unsigned rng = 0x12345678u;
    char msg[MSGSZ];
    for (long i = 0; i < ROUNDS; i++) {
        rng = rng * 1103515245u + 12345u;
        unsigned phase = (rng >> 16) & 7u;
        // mixture: 0 = immediate (tight drain/re-block window), else park the pump for a jittered spell
        long ns = (phase == 0) ? 0 : (long)((rng % 400u) + 1u) * 1000L; // 0 .. ~400us
        if (ns > 0) { struct timespec ts = {0, ns}; nanosleep(&ts, NULL); }
        memcpy(msg, &i, MSGSZ);
        ssize_t w = write(primary[0], msg, MSGSZ);
        if (w != MSGSZ) { perror("parent write primary"); return 4; }
        sent++;
        // occasional 2-message burst: guarantees the socket buffer is NON-EMPTY when the IO pump (one read
        // per level wake) hits its StopWatch->Watch window, so the re-ADD must re-prime a readable socket.
        if (phase == 1 && i + 1 < ROUNDS) {
            i++;
            memcpy(msg, &i, MSGSZ);
            if (write(primary[0], msg, MSGSZ) != MSGSZ) { perror("parent burst write"); return 4; }
            sent++;
        }
        // drain any pending ACK bytes (backpressure): keep outstanding bounded
        char a[64];
        ssize_t ar = read(node[0], a, sizeof a);
        if (ar > 0) acked += ar;
        // if we are too far ahead, block briefly for an ACK so the pump's parked-then-arrive window recurs
        while (sent - acked * 256 > 2048) {
            struct timespec ts = {0, 200 * 1000}; nanosleep(&ts, NULL);
            ar = read(node[0], a, sizeof a);
            if (ar > 0) acked += ar; else break;
        }
    }

    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("parent primary-pump sent=%ld child_exit=%d\n", sent, code);
    return code == 0 ? 0 : 1;
}
