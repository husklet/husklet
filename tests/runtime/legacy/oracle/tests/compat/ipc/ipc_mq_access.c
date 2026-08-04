// POSIX message-queue access-mode + descriptor-semantics fidelity, diffed vs the native oracle (aarch64) /
// qemu (x86_64). macOS has no POSIX mqueue kernel object, so hl emulates an in-process named priority
// queue; this pins the descriptor's O_ACCMODE enforcement and a few descriptor-lifetime facts to the real
// kernel. Verdict-only (errno NAMES + booleans, never raw descriptors), so hl must be byte-identical.
// Exercises: send on an O_RDONLY descriptor -> EBADF, receive on an O_WRONLY descriptor -> EBADF (both
// checked after the fd lookup, before EMSGSIZE); O_NONBLOCK reflected in mq_getattr immediately; mq_setattr
// changes only mq_flags (maxmsg/msgsize fixed at open) and returns the prior attr; a descriptor keeps working
// after mq_unlink; a zero-length message is a distinct message with curmsgs==1 and a 0-byte receive.
#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case EBADF: return "EBADF";
    case EAGAIN: return "EAGAIN";
    case EMSGSIZE: return "EMSGSIZE";
    case EINVAL: return "EINVAL";
    case ENOENT: return "ENOENT";
    default: return "OTHER";
    }
}

int main(void) {
    const char *name = "/hl_mq_access";
    mq_unlink(name);
    struct mq_attr at = {0};
    at.mq_maxmsg = 4;
    at.mq_msgsize = 16;

    // ---- access-mode enforcement ----
    // send on an O_RDONLY descriptor -> EBADF
    mqd_t rq = mq_open(name, O_CREAT | O_RDONLY, 0600, &at);
    printf("rdonly_open=%d\n", rq != (mqd_t)-1);
    printf("rdonly_send=%s\n", en(mq_send(rq, "x", 1, 0) == 0 ? 0 : errno));

    // receive on an O_WRONLY descriptor -> EBADF (the send here must still succeed and stay queued)
    mqd_t wq = mq_open(name, O_WRONLY);
    printf("wronly_open=%d\n", wq != (mqd_t)-1);
    printf("wronly_send=%s\n", en(mq_send(wq, "hello", 5, 3) == 0 ? 0 : errno));
    char rb[16];
    unsigned pp;
    printf("wronly_recv=%s\n", en(mq_receive(wq, rb, sizeof rb, &pp) < 0 ? errno : 0));

    // an O_RDWR descriptor can drain the message the O_WRONLY one queued
    mqd_t rw = mq_open(name, O_RDWR);
    ssize_t rn = mq_receive(rw, rb, sizeof rb, &pp);
    if (rn > 0) rb[rn] = 0;
    printf("rdwr_drain=%s msg=%s prio=%u\n", en(rn < 0 ? errno : 0), rn > 0 ? rb : "-", rn > 0 ? pp : 0);
    mq_close(rq);
    mq_close(wq);

    // ---- O_NONBLOCK reflected immediately in getattr ----
    mqd_t nq = mq_open(name, O_RDWR | O_NONBLOCK);
    struct mq_attr ga;
    mq_getattr(nq, &ga);
    printf("open_nonblock_flag=%d\n", (ga.mq_flags & O_NONBLOCK) != 0);
    mq_close(nq);

    // ---- mq_setattr changes only mq_flags; maxmsg/msgsize fixed; returns prior attr ----
    struct mq_attr na = {0}, old = {0};
    na.mq_flags = O_NONBLOCK;
    na.mq_maxmsg = 999; // must be ignored
    na.mq_msgsize = 999;
    int sr = mq_setattr(rw, &na, &old);
    struct mq_attr now;
    mq_getattr(rw, &now);
    printf("setattr_ret=%d old_max=%ld old_size=%ld old_flags=%ld\n", sr, old.mq_maxmsg, old.mq_msgsize,
           old.mq_flags);
    printf("after_max=%ld after_size=%ld after_nonblock=%d\n", now.mq_maxmsg, now.mq_msgsize,
           (now.mq_flags & O_NONBLOCK) != 0);

    // ---- descriptor survives mq_unlink ----
    mq_unlink(name);
    int su = mq_send(rw, "survive", 7, 2);
    char sb[16];
    unsigned sp;
    ssize_t sn = mq_receive(rw, sb, sizeof sb, &sp);
    if (sn > 0) sb[sn] = 0;
    printf("survive_send=%s survive_recv=%s msg=%s\n", en(su == 0 ? 0 : errno), en(sn < 0 ? errno : 0),
           sn > 0 ? sb : "-");
    printf("reopen_after_unlink=%s\n", en(mq_open(name, O_RDWR) == (mqd_t)-1 ? errno : 0));
    mq_close(rw);

    // ---- zero-length message is a distinct message ----
    const char *zn = "/hl_mq_access_zero";
    mq_unlink(zn);
    struct mq_attr zat = {0};
    zat.mq_maxmsg = 2;
    zat.mq_msgsize = 8;
    mqd_t zq = mq_open(zn, O_CREAT | O_RDWR, 0600, &zat);
    mq_send(zq, "", 0, 0);
    struct mq_attr zg;
    mq_getattr(zq, &zg);
    printf("zero_curmsgs=%ld\n", zg.mq_curmsgs);
    char zb[8];
    unsigned zp;
    printf("zero_recv_len=%zd\n", mq_receive(zq, zb, sizeof zb, &zp));
    mq_close(zq);
    printf("zero_unlink=%s\n", en(mq_unlink(zn) == 0 ? 0 : errno));
    return 0;
}
