// POSIX message queues: mq_open, priority ordering, mq_timedsend/timedreceive, mq_getattr.
#include <mqueue.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <errno.h>
#include <unistd.h>

int main(void) {
    char name[64];
    snprintf(name, sizeof name, "/hl_mq_%d", (int)getpid());
    mq_unlink(name);

    struct mq_attr attr = {0};
    attr.mq_maxmsg = 4;
    attr.mq_msgsize = 32;
    mqd_t q = mq_open(name, O_CREAT | O_RDWR, 0600, &attr);
    if (q == (mqd_t)-1) { printf("mqueue open=0\n"); return 0; }

    struct mq_attr got = {0};
    mq_getattr(q, &got);
    int attr_ok = got.mq_maxmsg == 4 && got.mq_msgsize == 32 && got.mq_curmsgs == 0;

    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_sec += 5;
    mq_timedsend(q, "low", 3, 1, &ts);
    mq_timedsend(q, "high", 4, 9, &ts);
    mq_timedsend(q, "mid", 3, 5, &ts);

    // Highest priority delivered first.
    char buf[32];
    unsigned prio = 0;
    ssize_t n1 = mq_timedreceive(q, buf, sizeof buf, &prio, &ts);
    int first_high = n1 == 4 && prio == 9 && memcmp(buf, "high", 4) == 0;
    ssize_t n2 = mq_timedreceive(q, buf, sizeof buf, &prio, &ts);
    int second_mid = n2 == 3 && prio == 5 && memcmp(buf, "mid", 3) == 0;

    // Empty queue times out (past deadline) with ETIMEDOUT.
    mq_timedreceive(q, buf, sizeof buf, &prio, &ts); // drain "low"
    struct timespec past = {0, 0};
    clock_gettime(CLOCK_REALTIME, &past);
    ssize_t n3 = mq_timedreceive(q, buf, sizeof buf, &prio, &past);
    int timeout = n3 < 0 && errno == ETIMEDOUT;

    mq_close(q);
    int unlinked = mq_unlink(name) == 0;
    printf("mqueue attr=%d high_first=%d mid_second=%d timeout=%d unlink=%d\n",
           attr_ok, first_high, second_mid, timeout, unlinked);
    return 0;
}
