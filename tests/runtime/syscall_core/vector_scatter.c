// Vectored I/O against descriptions that carry no vectored path of their own:
// each must gather every segment on write and scatter across every segment on read,
// rather than settling for the first segment and returning a short count.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

// eventfd hands back its whole 8-byte counter, which a readv must spread over both
// 4-byte segments. Its writev stays single-segment because Linux loops eventfd
// writes per segment and rejects a partial counter.
static int eventfd_case(void) {
    int fd = eventfd(0, 0);
    if (fd < 0) {
        return 0;
    }
    unsigned char value[8] = {0, 0, 0, 0, 0, 0, 0, 5};
    struct iovec whole[1] = {{value, 8}};
    long written = writev(fd, whole, 1);
    unsigned char low[4] = {0}, high[4] = {0};
    struct iovec halves[2] = {{low, 4}, {high, 4}};
    long got = readv(fd, halves, 2);
    close(fd);
    return written == 8 && got == 8 && high[3] == 5;
}

// A unix datagram carries one record, so a gathered writev must arrive whole.
static int socket_case(void) {
    int pair[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, pair)) {
        return 0;
    }
    struct iovec parts[3] = {{(void *)"record-", 7}, {(void *)"", 0}, {(void *)"tail", 4}};
    long written = writev(pair[1], parts, 3);
    char head[4] = {0}, rest[32] = {0};
    struct iovec halves[2] = {{head, 4}, {rest, 32}};
    long got = readv(pair[0], halves, 2);
    close(pair[0]);
    close(pair[1]);
    return written == 11 && got == 11 && !memcmp(head, "reco", 4) && !memcmp(rest, "rd-tail", 7);
}

// A procfs snapshot must scatter past a deliberately tiny leading segment.
static int procfs_case(void) {
    int fd = open("/proc/self/status", O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    char first[1] = {0}, rest[512] = {0};
    struct iovec parts[2] = {{first, 1}, {rest, 512}};
    long got = readv(fd, parts, 2);
    close(fd);
    return got > 1;
}

// A /dev builtin must do the same in both directions.
static int device_case(void) {
    int fd = open("/dev/zero", O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    char first[2], rest[30];
    memset(first, 1, sizeof(first));
    memset(rest, 1, sizeof(rest));
    struct iovec parts[2] = {{first, 2}, {rest, 30}};
    long got = readv(fd, parts, 2);
    close(fd);
    int sink = open("/dev/null", O_WRONLY);
    if (sink < 0) {
        return 0;
    }
    struct iovec out[2] = {{(void *)"ab", 2}, {(void *)"cde", 3}};
    long written = writev(sink, out, 2);
    close(sink);
    return got == 32 && !first[0] && !rest[29] && written == 5;
}

// A writable procfs node has no vectored write path, so its writev must still account
// for every segment. Only the count is checked: Linux applies each segment as its own
// write here, so the surviving comm value is the last segment rather than the join.
static int comm_case(void) {
    int fd = open("/proc/self/comm", O_WRONLY);
    if (fd < 0) {
        return 0;
    }
    struct iovec parts[2] = {{(void *)"abc", 3}, {(void *)"def", 3}};
    long written = writev(fd, parts, 2);
    close(fd);
    return written == 6;
}

int main(void) {
    int event_ok = eventfd_case();
    int socket_ok = socket_case();
    int procfs_ok = procfs_case();
    int device_ok = device_case();
    int comm_ok = comm_case();
    printf("vector-scatter event=%d socket=%d procfs=%d device=%d comm=%d\n", event_ok, socket_ok, procfs_ok,
           device_ok, comm_ok);
    return event_ok && socket_ok && procfs_ok && device_ok && comm_ok ? 0 : 2;
}
