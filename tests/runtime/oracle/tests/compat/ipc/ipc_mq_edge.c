// POSIX message-queue errno/edge fidelity (mq_open/mq_timedsend/mq_timedreceive/mq_getattr) — diffed vs
// the native oracle. Verdict-only (errno NAMES + booleans, never raw descriptors), so hl must be
// byte-identical to native Linux (aarch64) / qemu (x86_64). macOS has no POSIX mqueue kernel object, so
// hl emulates a named in-process priority queue; this pins that emulation to the real kernel's errnos.
// Exercises: ENOENT (open missing w/o O_CREAT), EEXIST (O_CREAT|O_EXCL on existing), ENAMETOOLONG, attr
// maxmsg/msgsize, EMSGSIZE (oversized send / undersized receive), O_NONBLOCK EAGAIN (recv-empty / send-full),
// curmsgs, strict highest-priority-first delivery, and the mq_timed{send,receive} blocking matrix
// EINVAL(tv_nsec)/ETIMEDOUT on a blocking (non-O_NONBLOCK) descriptor. (mq_notify lives in ipc_mq_notify.c,
// aarch64-only — qemu-user's mq_notify is not a faithful oracle.)
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case ENOENT: return "ENOENT";
    case EEXIST: return "EEXIST";
    case EMSGSIZE: return "EMSGSIZE";
    case EAGAIN: return "EAGAIN";
    case EINVAL: return "EINVAL";
    case EBUSY: return "EBUSY";
    case ETIMEDOUT: return "ETIMEDOUT";
    case ENAMETOOLONG: return "ENAMETOOLONG";
    default: return "OTHER";
    }
}

// A CLOCK_REALTIME deadline `ms` milliseconds from now (mq_timed* absolute timeouts are CLOCK_REALTIME).
static struct timespec deadline_in(long ms) {
    struct timespec t;
    clock_gettime(CLOCK_REALTIME, &t);
    t.tv_sec += ms / 1000;
    t.tv_nsec += (ms % 1000) * 1000000L;
    if (t.tv_nsec >= 1000000000L) {
        t.tv_nsec -= 1000000000L;
        t.tv_sec++;
    }
    return t;
}

int main(void) {
    const char *name = "/hl_mq_edge";
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

    // ---- ENAMETOOLONG: a name component longer than NAME_MAX(255) ----
    char toolong[300];
    toolong[0] = '/';
    memset(toolong + 1, 'a', 257);
    toolong[258] = 0; // 257-char component > 255
    printf("open_toolong=%s\n", en(mq_open(toolong, O_CREAT | O_RDWR, 0600, &at) == (mqd_t)-1 ? errno : 0));

    // ---- blocking (non-O_NONBLOCK) timed matrix: EINVAL(tv_nsec) / ETIMEDOUT ----
    const char *tn = "/hl_mq_timed";
    mq_unlink(tn);
    struct mq_attr tat = {0};
    tat.mq_maxmsg = 1;
    tat.mq_msgsize = 8;
    mqd_t tq = mq_open(tn, O_CREAT | O_RDWR, 0600, &tat); // NO O_NONBLOCK -> blocking descriptor
    if (tq == (mqd_t)-1) return 1;
    // send with an out-of-range tv_nsec is validated before the queue state -> EINVAL (queue has room here)
    struct timespec bad_ts = {0, 1000000000L};
    printf("tsend_einval=%s\n", en(mq_timedsend(tq, "x", 1, 0, &bad_ts) == 0 ? 0 : errno));
    // receive on the (still empty) queue with a bad tv_nsec -> EINVAL
    char tb[8];
    unsigned tp;
    printf("trecv_einval=%s\n", en(mq_timedreceive(tq, tb, sizeof tb, &tp, &bad_ts) < 0 ? errno : 0));
    // fill the single slot, then a blocking send with a short future deadline on the full queue -> ETIMEDOUT
    mq_send(tq, "a", 1, 0);
    struct timespec dl = deadline_in(50);
    printf("tsend_timeout=%s\n", en(mq_timedsend(tq, "b", 1, 0, &dl) == 0 ? 0 : errno));
    // drain it, then a blocking receive with a short future deadline on the empty queue -> ETIMEDOUT
    mq_receive(tq, tb, sizeof tb, &tp);
    dl = deadline_in(50);
    printf("trecv_timeout=%s\n", en(mq_timedreceive(tq, tb, sizeof tb, &tp, &dl) < 0 ? errno : 0));
    mq_close(tq);
    printf("tunlink=%s\n", en(mq_unlink(tn) == 0 ? 0 : errno));
    return 0;
}
