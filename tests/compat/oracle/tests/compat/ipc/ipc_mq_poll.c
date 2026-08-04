// poll(2) on a POSIX message-queue descriptor reflects real queue readiness: an empty queue is not
// POLLIN (but is POLLOUT, there is room), a non-empty queue is POLLIN, and a full queue is not POLLOUT.
// A real host mqd is a kernel-pollable object; the former /dev/null-backed broker descriptor always
// reported POLLIN|POLLOUT regardless of depth. Diffed vs the committed golden (same handler both arches).
#include <fcntl.h>
#include <mqueue.h>
#include <poll.h>
#include <stdio.h>
#include <unistd.h>

static int poll_bit(int fd, short want) {
    struct pollfd p = {fd, want, 0};
    return poll(&p, 1, 0) > 0 && (p.revents & want) != 0;
}

int main(void) {
    const char *nn = "/hl_mq_poll";
    mq_unlink(nn);
    struct mq_attr at = {0};
    at.mq_maxmsg = 2;
    at.mq_msgsize = 8;
    mqd_t q = mq_open(nn, O_CREAT | O_RDWR | O_NONBLOCK, 0600, &at);
    printf("open=%d\n", q != (mqd_t)-1);
    if (q == (mqd_t)-1) return 1;

    printf("empty_pollin=%d\n", poll_bit(q, POLLIN));   // 0: nothing to read
    printf("empty_pollout=%d\n", poll_bit(q, POLLOUT)); // 1: room to send
    mq_send(q, "a", 1, 0);
    printf("nonempty_pollin=%d\n", poll_bit(q, POLLIN)); // 1: a message pends
    mq_send(q, "b", 1, 0);                               // queue now full (maxmsg=2)
    printf("full_pollin=%d\n", poll_bit(q, POLLIN));     // 1
    printf("full_pollout=%d\n", poll_bit(q, POLLOUT));   // 0: no room
    char buf[8];
    unsigned pr;
    mq_receive(q, buf, 8, &pr);
    mq_receive(q, buf, 8, &pr);
    printf("drained_pollin=%d\n", poll_bit(q, POLLIN));   // 0
    printf("drained_pollout=%d\n", poll_bit(q, POLLOUT)); // 1
    mq_close(q);
    printf("unlink=%s\n", mq_unlink(nn) == 0 ? "ok" : "err");
    return 0;
}
