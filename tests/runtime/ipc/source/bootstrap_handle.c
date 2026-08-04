// multi-process application child-process IPC BOOTSTRAP shape end-to-end: the coordinator hands a freshly launched child its
// initial platform channel + a shared-memory command buffer at LAUNCH time by (a) placing each fd at a
// FIXED number and (b) passing that number as a command-line STRING (multi-process application's --mojo-platform-channel-
// handle=N / GlobalDescriptors convention). The child, after fork + in-place execve, must find each fd
// alive AT THE NUMBER THE ARGV NAMED and be able to use it. The existing xproc-inbound/zygote/scm-futex
// gates prove the cross-process TRANSPORT (socketpair/eventfd/epoll/SCM_RIGHTS/futex) delivers, but none
// carries a memfd across hl's fork+in-place-execve, mmaps it in the NEW image, and wakes it by futex --
// nor validates that the cmdline fd NUMBER and the surviving fd agree. This closes that bootstrap gap.
//
// What it asserts (coordinator == parent, child == exec'd worker/service/utility process):
//   1. chan   : the SEQPACKET IPC channel dup2'd to fd 7 (non-CLOEXEC) survives the in-place execve and
//               the child receives the coordinator's INBOUND message on it (the EstablishPeerChannel direction).
//   2. shmem  : a memfd command buffer dup2'd to fd 8 (non-CLOEXEC) survives the execve and is MAP_SHARED-
//               mmap'able in the new image, coherent with the coordinator's own independent mapping.
//   3. futex  : a FUTEX_WAKE issued through the coordinator's mapping releases the child parked in FUTEX_WAIT
//               on a word inside that shared page (the worker<->service command-buffer wakeup).
//   4. decoy  : a sibling fd left FD_CLOEXEC is GONE after the execve -- proving the close-on-exec sweep
//               actually ran, so (1)/(2) survive because hl honours the CLEARED cloexec flag, not because
//               the sweep is a no-op that would also leak a fd multi-process application meant to drop.
// A dropped/renumbered bootstrap fd (the multi-process dormant-worker hypothesis) fails deterministically
// (chan/shmem/futex=0) inside a timed wait -- never a hang.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/futex.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// Fixed child fd numbers the coordinator assigns and names on the command line (mirrors multi-process application's fd remap +
// base::GlobalDescriptors kBaseDescriptor offset -- deliberately above stdio, not 3/4).
#define CH_CHAN  7
#define CH_MEMFD 8

static long fwait(int *a, int e, const struct timespec *to) { return syscall(SYS_futex, a, FUTEX_WAIT, e, to, NULL, 0); }
static long fwake(int *a, int n) { return syscall(SYS_futex, a, FUTEX_WAKE, n, NULL, NULL, 0); }

static int argi(int argc, char **argv, const char *key) {
    size_t kl = strlen(key);
    for (int i = 1; i < argc; i++)
        if (!strncmp(argv[i], key, kl)) return atoi(argv[i] + kl);
    return -1;
}

// The exec'd child: recover the bootstrap purely from the argv-named fd numbers.
static int child_main(int argc, char **argv) {
    int chan = argi(argc, argv, "--chan=");
    int memfd = argi(argc, argv, "--memfd=");
    int decoy = argi(argc, argv, "--decoy=");

    // (4) the CLOEXEC sibling must have been swept by our own execve.
    int decoy_swept = (decoy < 0) ? 0 : (fcntl(decoy, F_GETFD) < 0 && errno == EBADF);

    // (2) the shared command buffer must still be the same object, mmap'able in this fresh image.
    int shmem_ok = 0;
    int *C = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0);
    if (C != MAP_FAILED) shmem_ok = 1;

    // (1) park an epoll on the channel exactly as a worker's IO thread would.
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ce = {.events = EPOLLIN, .data.fd = chan};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, chan, &ce) != 0) {
        printf("child bootstrap chan=0 (epoll_ctl %s)\n", strerror(errno));
        return 21;
    }
    char rdy = 'R';
    (void)!write(chan, &rdy, 1); // "launched + parked"
    int chan_ok = 0, msg_ok = 0;
    char msg[64];
    msg[0] = 0;
    struct epoll_event out[1];
    if (epoll_wait(ep, out, 1, 3000) == 1 && out[0].data.fd == chan) {
        ssize_t r = read(chan, msg, sizeof msg - 1);
        if (r > 0) {
            msg[r] = 0;
            chan_ok = 1;
            msg_ok = !strcmp(msg, "EstablishPeer");
        }
    }
    char parked = 'P';
    (void)!write(chan, &parked, 1); // tell the coordinator we consumed the channel msg and are about to futex

    // (3) block on the command-buffer word until the coordinator stores + FUTEX_WAKEs through its own mapping.
    int futex_ok = 0;
    if (shmem_ok) {
        struct timespec to = {3, 0};
        for (;;) {
            if (atomic_load((_Atomic int *)C) != 0) { futex_ok = 1; break; }
            long rc = fwait(C, 0, &to);
            if (rc == 0) { futex_ok = 1; break; }
            if (rc == -1 && errno == ETIMEDOUT) { futex_ok = 0; break; }
        }
    }

    printf("child bootstrap chan=%d msg=%s shmem=%d futex=%d decoy_swept=%d\n",
           chan_ok, msg_ok ? "EstablishPeer" : "?", shmem_ok, futex_ok, decoy_swept);
    fflush(stdout);
    return (chan_ok && msg_ok && shmem_ok && futex_ok && decoy_swept) ? 0 : 25;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) return child_main(argc, argv);

    // Coordinator side: create the IPC channel + the shared command buffer.
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) { perror("socketpair"); return 1; }
    int shm = (int)syscall(SYS_memfd_create, "cmdbuf", 0u);
    if (shm < 0 || ftruncate(shm, 4096) != 0) { perror("memfd"); return 1; }
    int *P = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
    if (P == MAP_FAILED) { perror("mmap"); return 1; }
    P[0] = 0;

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }
    if (pid == 0) {
        // Launch-time fd shuffle: place each bootstrap fd at its assigned number, non-CLOEXEC, and leave a
        // sibling CLOEXEC dup that the execve MUST drop. Then exec, naming the numbers on the command line.
        close(sv[0]);
        if (dup2(sv[1], CH_CHAN) < 0 || dup2(shm, CH_MEMFD) < 0) _exit(40);
        fcntl(CH_CHAN, F_SETFD, 0);
        fcntl(CH_MEMFD, F_SETFD, 0);
        int decoy = fcntl(shm, F_DUPFD_CLOEXEC, 20); // a CLOEXEC alias of the buffer, high fd number
        if (decoy < 0) _exit(41);
        if (sv[1] != CH_CHAN) close(sv[1]);
        if (shm != CH_MEMFD) close(shm);
        char self[512];
        ssize_t sl = readlink("/proc/self/exe", self, sizeof self - 1);
        if (sl <= 0) _exit(42);
        self[sl] = 0;
        char achan[32], amemfd[32], adecoy[32];
        snprintf(achan, sizeof achan, "--chan=%d", CH_CHAN);
        snprintf(amemfd, sizeof amemfd, "--memfd=%d", CH_MEMFD);
        snprintf(adecoy, sizeof adecoy, "--decoy=%d", decoy);
        char *av[] = {self, (char *)"child", achan, amemfd, adecoy, NULL};
        execv(self, av);
        _exit(43);
    }

    close(sv[1]);
    // Wait for the child's readiness datagram so the message we send is genuinely INBOUND to a parked epoll.
    char r = 0;
    if (read(sv[0], &r, 1) != 1 || r != 'R') { printf("parent ready-read r=%d\n", r); return 2; }
    struct timespec settle = {0, 50 * 1000 * 1000};
    nanosleep(&settle, NULL);
    const char *m = "EstablishPeer";
    if (write(sv[0], m, strlen(m)) < 0) { perror("write chan"); return 3; }

    // Wait for the child's "parked" datagram (channel msg consumed, about to FUTEX_WAIT), then wake it
    // through the coordinator's OWN independent mapping of the shared page.
    char pk = 0;
    if (read(sv[0], &pk, 1) != 1 || pk != 'P') { printf("parent park-read pk=%d\n", pk); return 4; }
    nanosleep(&settle, NULL);
    atomic_store((_Atomic int *)&P[0], 1);
    fwake(&P[0], INT_MAX);

    int st = 0;
    waitpid(pid, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -1;
    printf("parent bootstrap child_exit=%d\n", code);
    fflush(stdout);
    return code == 0 ? 0 : 1;
}
