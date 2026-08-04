// Cross-process POSIX message queue: a fork child sends by NAME to a queue the parent operates on.
// A real POSIX mqueue is a kernel object shared across fork by name, so (A) the parent's BLOCKING
// mq_receive on the empty queue wakes on the child's cross-process send, and (B) messages the child
// enqueues at distinct priorities are drained by the parent in priority order. (The former in-process
// broker gave the fork child its own copy of the queue, so the parent's blocking receive never woke.)
// Two pipes serialize the phases so the output is deterministic. Diffed vs the committed golden; hl runs
// the same host-passthrough handler on both guest arches.
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    const char *nn = "/hl_mq_xproc";
    mq_unlink(nn);
    struct mq_attr at = {0};
    at.mq_maxmsg = 4;
    at.mq_msgsize = 32;
    mqd_t q = mq_open(nn, O_CREAT | O_RDWR, 0600, &at);
    printf("open=%d\n", q != (mqd_t)-1);
    if (q == (mqd_t)-1) return 1;

    int p2c[2], c2p[2];
    if (pipe(p2c) != 0 || pipe(c2p) != 0) return 1;

    pid_t pid = fork();
    if (pid == 0) {
        close(p2c[1]);
        close(c2p[0]);
        mqd_t cq = mq_open(nn, O_WRONLY); // reopen the SAME named queue from the child
        if (cq == (mqd_t)-1) _exit(2);
        // phase A: a single send to wake the parent's blocking receive
        struct timespec s = {0, 50 * 1000 * 1000};
        nanosleep(&s, NULL);
        if (mq_send(cq, "wake", 4, 0) != 0) _exit(3);
        // phase B: wait for the parent's go-ahead, then enqueue three prioritized messages
        char g;
        if (read(p2c[0], &g, 1) != 1) _exit(4);
        if (mq_send(cq, "lo", 2, 1) != 0) _exit(5);
        if (mq_send(cq, "hi", 2, 7) != 0) _exit(6);
        if (mq_send(cq, "mid", 3, 3) != 0) _exit(7);
        char one = 'x';
        if (write(c2p[1], &one, 1) != 1) _exit(8);
        mq_close(cq);
        _exit(0);
    }
    close(p2c[0]);
    close(c2p[1]);

    char buf[64];
    unsigned prio;
    // phase A: BLOCKING receive on the empty queue -- wakes on the child's cross-process send
    ssize_t n = mq_receive(q, buf, 64, &prio);
    printf("wake len=%zd prio=%u msg=%.4s\n", n, prio, buf);

    // phase B: release the child, wait until all three are enqueued, then drain in priority order
    char one = 'x';
    if (write(p2c[1], &one, 1) != 1) return 1;
    char b;
    if (read(c2p[0], &b, 1) != 1) return 1;
    for (int i = 0; i < 3; i++) {
        ssize_t m = mq_receive(q, buf, 64, &prio);
        printf("drain%d prio=%u msg=%.*s\n", i, prio, (int)m, buf);
    }

    int st = 0;
    waitpid(pid, &st, 0);
    printf("child_exit=%d\n", WIFEXITED(st) && WEXITSTATUS(st) == 0);
    mq_close(q);
    printf("unlink=%s\n", mq_unlink(nn) == 0 ? "ok" : "err");
    return 0;
}
