// #396 signal/timeout corners for poll/select/pselect that have no portable POSIX ground truth on a
// linux dev host, so this is diffed against a native oracle (aarch64 direct / x86 under qemu).
//
// The headline check is the select02 HANG regression: a signal hl hooks (host_sigh installed) but the
// guest has BLOCKED must NOT restart the full timeout on the resulting EINTR. Before the fix, each such
// spurious wakeup re-armed the entire timeout, so a *repeating* blocked signal made select/poll/pselect
// overshoot without bound (LTP select02 never returned). Here a child hammers a blocked SIGUSR1 through
// a 300ms wait; a correct kernel ignores it and returns ~on time, so "capped" (elapsed under a generous
// bound) must be 1 -- exactly as native. Also covers EINTR on a delivered handler and EFAULT/EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long long mono_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (long long)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}
static volatile int got;
static void onsig(int s) { (void)s; got = 1; }

// Child that raps `sig` on `parent` every 40ms for ~4s (long enough that a full-timeout-restart bug
// grossly overshoots the 300ms wait), then exits.
static pid_t pest(pid_t parent, int sig) {
    pid_t p = fork();
    if (p == 0) {
        for (int i = 0; i < 100; i++) { usleep(40000); kill(parent, sig); }
        _exit(0);
    }
    return p;
}

int main(void) {
    int fds[2];
    if (pipe(fds) < 0) return 1; // never readable
    pid_t self = getpid();

    // Install a handler for SIGUSR1 (so hl hooks it via host_sigh) then BLOCK it. A blocked signal must
    // not interrupt the wait as far as the guest is concerned; hl must re-block for the REMAINING time.
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = onsig;
    sigaction(SIGUSR1, &sa, NULL);
    sigset_t bl;
    sigemptyset(&bl);
    sigaddset(&bl, SIGUSR1);
    sigprocmask(SIG_BLOCK, &bl, NULL);

    // select under the signal storm: must return 0 within a generous cap (no unbounded restart).
    {
        pid_t c = pest(self, SIGUSR1);
        fd_set rs; FD_ZERO(&rs); FD_SET(fds[0], &rs);
        struct timeval tv = {0, 300000};
        long long t0 = mono_ms();
        int r = select(fds[0] + 1, &rs, NULL, NULL, &tv);
        long long dt = mono_ms() - t0;
        printf("select r=%d capped=%d\n", r, r == 0 && dt < 1500);
        kill(c, SIGKILL); waitpid(c, NULL, 0);
    }
    {
        pid_t c = pest(self, SIGUSR1);
        struct pollfd p = {.fd = fds[0], .events = POLLIN};
        long long t0 = mono_ms();
        int r = poll(&p, 1, 300);
        long long dt = mono_ms() - t0;
        printf("poll r=%d capped=%d\n", r, r == 0 && dt < 1500);
        kill(c, SIGKILL); waitpid(c, NULL, 0);
    }
    {
        pid_t c = pest(self, SIGUSR1);
        fd_set rs; FD_ZERO(&rs); FD_SET(fds[0], &rs);
        struct timespec ts = {0, 300000000};
        long long t0 = mono_ms();
        int r = pselect(fds[0] + 1, &rs, NULL, NULL, &ts, NULL);
        long long dt = mono_ms() - t0;
        printf("pselect r=%d capped=%d\n", r, r == 0 && dt < 1500);
        kill(c, SIGKILL); waitpid(c, NULL, 0);
    }
    // ppoll (Linux-only): readiness, a finite timeout returning 0, and the same no-restart guarantee.
    {
        if (write(fds[1], "y", 1) < 0) return 1;
        struct pollfd p = {.fd = fds[0], .events = POLLIN};
        struct timespec ts = {1, 0};
        int ppoll_rd = (ppoll(&p, 1, &ts, NULL) == 1) && (p.revents & POLLIN);
        char d; if (read(fds[0], &d, 1) < 0) return 1; // drain -> not readable again
        struct pollfd pn = {.fd = fds[0], .events = POLLIN};
        struct timespec tz = {0, 30000000};
        int ppoll_to = (ppoll(&pn, 1, &tz, NULL) == 0);
        pid_t c = pest(self, SIGUSR1);
        struct pollfd pp = {.fd = fds[0], .events = POLLIN};
        struct timespec ts2 = {0, 300000000};
        long long t0 = mono_ms();
        int r = ppoll(&pp, 1, &ts2, NULL);
        long long dt = mono_ms() - t0;
        printf("ppoll rd=%d to=%d capped=%d\n", ppoll_rd, ppoll_to, r == 0 && dt < 1500);
        kill(c, SIGKILL); waitpid(c, NULL, 0);
    }

    // A DELIVERED handler (unblocked, no SA_RESTART) must interrupt with EINTR (never restarted).
    sigprocmask(SIG_UNBLOCK, &bl, NULL);
    {
        pid_t c = pest(self, SIGUSR1);
        struct pollfd p = {.fd = fds[0], .events = POLLIN};
        got = 0; errno = 0;
        int r = poll(&p, 1, 5000);
        int e = errno;
        printf("eintr r=%d eintr=%d gotsig=%d\n", r, r < 0 && e == EINTR, got);
        kill(c, SIGKILL); waitpid(c, NULL, 0);
    }

    // EFAULT: a faulty timeout pointer. EINVAL: negative nfds, and a malformed (negative) timeout.
    errno = 0;
    int efault = poll((struct pollfd *)(void *)0x1, 1, 0) < 0 && errno == EFAULT;
    struct timeval bad = {0, 0};
    errno = 0;
    int einval_nfds = select(-1, NULL, NULL, NULL, &bad) < 0 && errno == EINVAL;
    struct timeval neg = {-1, 0};
    errno = 0;
    int einval_to = select(0, NULL, NULL, NULL, &neg) < 0 && errno == EINVAL;
    printf("edges efault=%d einval_nfds=%d einval_to=%d\n", efault, einval_nfds, einval_to);
    return 0;
}
