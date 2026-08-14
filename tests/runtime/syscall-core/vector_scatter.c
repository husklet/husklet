// Vectored I/O against descriptions that carry no vectored path of their own:
// each must gather every segment on write and scatter across every segment on read,
// rather than settling for the first segment and returning a short count.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

// eventfd readv scatters one 8-byte scalar across vectors. A failed copy follows
// Linux's copy_to_iter ordering and consumes the counter before reporting EFAULT.
static int eventfd_case(void) {
    int fd = eventfd(0, EFD_NONBLOCK);
    if (fd < 0) return 0;
    uint64_t value = 5, readback = 0;
    struct iovec whole_write = {&value, 8};
    struct iovec split_read[2] = {{&readback, 4}, {(char *)&readback + 4, 4}};
    int ok = writev(fd, &whole_write, 1) == 8 && readv(fd, split_read, 2) == 8 && readback == 5;

    errno = 0;
    ok &= readv(fd, split_read, 2) == -1 && errno == EAGAIN;
    struct iovec split_write[2] = {{&value, 4}, {(char *)&value + 4, 4}};
    errno = 0;
    ok &= writev(fd, split_write, 2) == -1 && errno == EINVAL;
    struct iovec zero_split_write[3] = {{&value, 0}, {&value, 8}, {(char *)&value + 8, 0}};
    errno = 0;
    ok &= writev(fd, zero_split_write, 3) == -1 && errno == EINVAL;
    value = 6;
    unsigned char long_buffer[9] = {0};
    struct iovec long_read = {long_buffer, sizeof long_buffer};
    ok &= writev(fd, &whole_write, 1) == 8 && readv(fd, &long_read, 1) == 8;
    memcpy(&readback, long_buffer, sizeof readback);
    ok &= readback == 6;
    struct iovec short_vector = {&value, 7};
    errno = 0;
    ok &= writev(fd, &short_vector, 1) == -1 && errno == EINVAL;
    errno = 0;
    ok &= readv(fd, &short_vector, 1) == -1 && errno == EINVAL;
    value = UINT64_MAX;
    errno = 0;
    ok &= writev(fd, &whole_write, 1) == -1 && errno == EINVAL;

    value = 7;
    ok &= writev(fd, &whole_write, 1) == 8;
    struct iovec bad_read[2] = {{(void *)1, 4}, {(char *)&readback + 4, 4}};
    errno = 0;
    ok &= readv(fd, bad_read, 2) == -1 && errno == EFAULT;
    readback = 0;
    errno = 0;
    ok &= readv(fd, split_read, 2) == -1 && errno == EAGAIN;
    close(fd);

    fd = eventfd(2, EFD_NONBLOCK | EFD_SEMAPHORE);
    if (fd < 0) return 0;
    readback = 0;
    ok &= readv(fd, split_read, 2) == 8 && readback == 1;
    readback = 0;
    ok &= readv(fd, split_read, 2) == 8 && readback == 1;
    errno = 0;
    ok &= readv(fd, split_read, 2) == -1 && errno == EAGAIN;
    close(fd);
    return ok;
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
