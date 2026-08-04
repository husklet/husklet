// POSIX mq_notify fidelity — diffed vs the native aarch64 oracle only. (qemu-user's mq_notify does NOT
// faithfully emulate the SIGEV notification, so it is not a usable x86 oracle; hl runs the SAME arch-
// normalized handler for both guest arches, so the aarch64 real-kernel diff validates the x86 path too.)
// macOS has no POSIX mqueue kernel object; hl emulates a named in-process priority queue and delivers the
// one-shot notification on the queue's empty->non-empty edge. Exercises: SIGEV_NONE register -> 0, a second
// register on the owned queue -> EBUSY, unregister (NULL) -> 0, and a SIGEV_SIGNAL registration that fires
// an SI_MESGQ signal carrying the registered sigev_value when a message arrives on the empty queue.
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case EBUSY: return "EBUSY";
    case EBADF: return "EBADF";
    case EINVAL: return "EINVAL";
    default: return "OTHER";
    }
}

#define NOTIFY_DATA 0x12345678
static volatile sig_atomic_t g_notified;
static volatile int g_ncode, g_nval, g_nsigno, g_npid;
static void notify_handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    g_nsigno = sig;
    g_ncode = si->si_code;
    g_nval = si->si_value.sival_int;
    g_npid = si->si_pid;
    g_notified = 1;
}

int main(void) {
    const char *nn = "/hl_mq_notify";
    mq_unlink(nn);
    struct mq_attr at = {0};
    at.mq_maxmsg = 4;
    at.mq_msgsize = 16;
    mqd_t nq = mq_open(nn, O_CREAT | O_RDWR, 0600, &at);
    printf("open=%d\n", nq != (mqd_t)-1);
    if (nq == (mqd_t)-1) return 1;

    // register SIGEV_NONE -> 0; a second registration while owned -> EBUSY; unregister (NULL) -> 0
    struct sigevent sev;
    memset(&sev, 0, sizeof sev);
    sev.sigev_notify = SIGEV_NONE;
    printf("notify_none=%s\n", en(mq_notify(nq, &sev) == 0 ? 0 : errno));
    printf("notify_busy=%s\n", en(mq_notify(nq, &sev) == 0 ? 0 : errno));
    printf("notify_unreg=%s\n", en(mq_notify(nq, NULL) == 0 ? 0 : errno));

    // bad sigev_notify value -> EINVAL
    memset(&sev, 0, sizeof sev);
    sev.sigev_notify = 999;
    printf("notify_badnotify=%s\n", en(mq_notify(nq, &sev) == 0 ? 0 : errno));

    // SIGEV_SIGNAL: a message on the empty queue fires an SI_MESGQ signal with the registered sival_int
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = notify_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR1, &sa, NULL);
    memset(&sev, 0, sizeof sev);
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGUSR1;
    sev.sigev_value.sival_int = NOTIFY_DATA;
    g_notified = 0;
    printf("notify_sig_reg=%s\n", en(mq_notify(nq, &sev) == 0 ? 0 : errno));
    mq_send(nq, "ping", 4, 0); // empty->non-empty edge fires the notification
    for (int i = 0; i < 200 && !g_notified; i++) {
        struct timespec s = {0, 10 * 1000 * 1000};
        nanosleep(&s, NULL);
    }
    printf("notify_fired=%d\n", (int)g_notified);
    printf("notify_signo=%d\n", g_nsigno == SIGUSR1);
    printf("notify_code=%d\n", g_ncode == SI_MESGQ);
    printf("notify_value=%d\n", g_nval == NOTIFY_DATA);
    printf("notify_pid=%d\n", g_npid == getpid());

    // after firing, the one-shot registration is consumed -> re-register succeeds (not EBUSY)
    memset(&sev, 0, sizeof sev);
    sev.sigev_notify = SIGEV_NONE;
    printf("notify_reregister=%s\n", en(mq_notify(nq, &sev) == 0 ? 0 : errno));

    mq_close(nq);
    printf("unlink=%s\n", en(mq_unlink(nn) == 0 ? 0 : errno));
    return 0;
}
