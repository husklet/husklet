// POSIX message-queue errno/edge fidelity (mq_open/mq_timedsend/mq_timedreceive/mq_getattr) — diffed
// vs the native oracle. Verdict-only (errno NAMES + booleans, never raw descriptors), so dd must be
// byte-identical to native Linux (aarch64) / qemu (x86_64). macOS has no POSIX mqueue kernel object, so
// dd emulates a named in-process priority queue; this pins that emulation to the real kernel's errnos.
// Exercises: ENOENT (open missing w/o O_CREAT), EEXIST (O_CREAT|O_EXCL on existing), attr maxmsg/msgsize,
// EMSGSIZE (oversized send / undersized receive), O_NONBLOCK EAGAIN (recv-empty / send-full), curmsgs,
// and strict highest-priority-first delivery.
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <string.h>

static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case ENOENT: return "ENOENT";
    case EEXIST: return "EEXIST";
    case EMSGSIZE: return "EMSGSIZE";
    case EAGAIN: return "EAGAIN";
    case EINVAL: return "EINVAL";
    default: return "OTHER";
    }
}

int main(void) {
    const char *name = "/dd_mq_edge";
    mq_unlink(name); // clean slate

    // open a missing queue without O_CREAT -> ENOENT
    mqd_t bad = mq_open(name, O_RDWR);
    printf("open_missing=%s\n", en(bad == (mqd_t)-1 ? errno : 0));

    struct mq_attr at = {0};
    at.mq_maxmsg = 4;
    at.mq_msgsize = 16;
    mqd_t q = mq_open(name, O_CREAT | O_RDWR | O_NONBLOCK, 0600, &at);
    printf("open_create=%d\n", q != (mqd_t)-1);
    if (q == (mqd_t)-1) return 1;

    // O_CREAT|O_EXCL on the now-existing queue -> EEXIST
    mqd_t ex = mq_open(name, O_CREAT | O_EXCL | O_RDWR, 0600, &at);
    printf("open_excl=%s\n", en(ex == (mqd_t)-1 ? errno : 0));

    // getattr reports the queue geometry we asked for
    struct mq_attr got;
    mq_getattr(q, &got);
    printf("attr=%d\n", got.mq_maxmsg == 4 && got.mq_msgsize == 16 && got.mq_curmsgs == 0);

    // send larger than mq_msgsize -> EMSGSIZE
    char big[32] = {0};
    printf("send_big=%s\n", en(mq_send(q, big, sizeof big, 0) == 0 ? 0 : errno));

    // receive on an empty queue (O_NONBLOCK) -> EAGAIN
    char rbuf[16];
    unsigned prio;
    printf("recv_empty=%s\n", en(mq_receive(q, rbuf, sizeof rbuf, &prio) < 0 ? errno : 0));

    // send three at different priorities, then a fourth to fill (maxmsg=4)
    mq_send(q, "low", 3, 1);
    mq_send(q, "high", 4, 9);
    mq_send(q, "mid", 3, 5);
    mq_send(q, "base", 4, 0);
    mq_getattr(q, &got);
    printf("full_curmsgs=%d\n", (int)got.mq_curmsgs);

    // a fifth send on a full O_NONBLOCK queue -> EAGAIN
    printf("send_full=%s\n", en(mq_send(q, "x", 1, 0) == 0 ? 0 : errno));

    // receive into a buffer smaller than mq_msgsize -> EMSGSIZE
    char tiny[4];
    printf("recv_small=%s\n", en(mq_receive(q, tiny, sizeof tiny, &prio) < 0 ? errno : 0));

    // drain in strict highest-priority-first order
    char order[64] = {0};
    for (int i = 0; i < 4; i++) {
        char b[16];
        ssize_t n = mq_receive(q, b, sizeof b, &prio);
        if (n < 0) break;
        b[n] = 0;
        char part[24];
        snprintf(part, sizeof part, "%s/%u ", b, prio);
        strcat(order, part);
    }
    printf("order=%s\n", order);
    mq_getattr(q, &got);
    printf("drained=%d\n", (int)got.mq_curmsgs);

    mq_close(q);
    printf("unlink=%s\n", en(mq_unlink(name) == 0 ? 0 : errno));
    // re-open after unlink without O_CREAT -> ENOENT
    printf("open_after_unlink=%s\n", en(mq_open(name, O_RDWR) == (mqd_t)-1 ? errno : 0));
    return 0;
}
