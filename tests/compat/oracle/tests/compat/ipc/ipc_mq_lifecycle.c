#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

enum { DESCRIPTORS = 65, CYCLES = 17 };

int main(void) {
    const char *name = "/hl_mq_lifecycle";
    struct mq_attr attributes = {0};
    struct mq_attr current;
    struct mq_attr nonblock = {0};
    mqd_t descriptors[DESCRIPTORS];
    int capacity_ok = 1;
    int duplicate_ok = 0;
    int cycles_ok = 1;
    int stale_ok = 0;

    attributes.mq_maxmsg = 4;
    attributes.mq_msgsize = 32;
    mq_unlink(name);
    descriptors[0] = mq_open(name, O_CREAT | O_RDWR, 0600, &attributes);
    if (descriptors[0] == (mqd_t)-1) return 1;
    for (int index = 1; index < DESCRIPTORS; ++index) {
        descriptors[index] = mq_open(name, O_RDWR);
        if (descriptors[index] == (mqd_t)-1) capacity_ok = 0;
    }
    for (int index = 0; index < DESCRIPTORS; ++index) {
        memset(&current, 0, sizeof(current));
        if (descriptors[index] == (mqd_t)-1 || mq_getattr(descriptors[index], &current) != 0 ||
            current.mq_maxmsg != attributes.mq_maxmsg || current.mq_msgsize != attributes.mq_msgsize)
            capacity_ok = 0;
    }
    {
        int duplicate = dup((int)descriptors[0]);
        nonblock.mq_flags = O_NONBLOCK;
        if (duplicate >= 0 && mq_setattr((mqd_t)duplicate, &nonblock, NULL) == 0 &&
            mq_getattr(descriptors[0], &current) == 0 && (current.mq_flags & O_NONBLOCK) != 0 &&
            mq_close((mqd_t)duplicate) == 0)
            duplicate_ok = 1;
    }
    if (mq_unlink(name) != 0) capacity_ok = 0;
    for (int index = 0; index < DESCRIPTORS; ++index)
        if (descriptors[index] != (mqd_t)-1 && mq_close(descriptors[index]) != 0) capacity_ok = 0;

    for (int index = 0; index < CYCLES; ++index) {
        char cycle_name[64];
        mqd_t queue;
        snprintf(cycle_name, sizeof(cycle_name), "/hl_mq_cycle_%d", index);
        mq_unlink(cycle_name);
        queue = mq_open(cycle_name, O_CREAT | O_RDWR, 0600, &attributes);
        if (queue == (mqd_t)-1 || mq_unlink(cycle_name) != 0 || mq_close(queue) != 0) cycles_ok = 0;
    }

    {
        mqd_t queue;
        int released;
        int plain;
        mq_unlink(name);
        queue = mq_open(name, O_CREAT | O_RDWR, 0600, &attributes);
        if (queue == (mqd_t)-1) return 2;
        released = (int)queue;
        if (mq_unlink(name) != 0 || mq_close(queue) != 0) return 3;
        plain = open("/dev/null", O_RDONLY | O_CLOEXEC);
        errno = 0;
        stale_ok = plain == released && mq_getattr((mqd_t)plain, &current) == -1 && errno == EBADF;
        if (plain >= 0) close(plain);
    }

    printf("mq_lifecycle capacity=%d duplicate=%d cycles=%d stale=%d\n", capacity_ok, duplicate_ok, cycles_ok,
           stale_ok);
    return capacity_ok && duplicate_ok && cycles_ok && stale_ok ? 0 : 4;
}
