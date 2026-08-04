#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef HL_PERF_OP
#error "HL_PERF_OP must select an operation"
#endif

enum { OP_MMAP = 1, OP_FILE, OP_PIPE, OP_EVENT, OP_IPC_LATENCY, OP_IPC_THROUGHPUT };

static int full_write(int fd, const void *buffer, size_t size) {
    const unsigned char *cursor = buffer;
    while (size != 0) {
        ssize_t written = write(fd, cursor, size);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) return -1;
        cursor += (size_t)written;
        size -= (size_t)written;
    }
    return 0;
}

static int full_read(int fd, void *buffer, size_t size) {
    unsigned char *cursor = buffer;
    while (size != 0) {
        ssize_t got = read(fd, cursor, size);
        if (got < 0 && errno == EINTR) continue;
        if (got <= 0) return -1;
        cursor += (size_t)got;
        size -= (size_t)got;
    }
    return 0;
}

static int wait_ok(pid_t child) {
    int status;
    pid_t got;
    do
        got = waitpid(child, &status, 0);
    while (got < 0 && errno == EINTR);
    return got == child && WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

static int perf_mmap(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    for (unsigned i = 0; i < 10000; ++i) {
        unsigned char *p = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) return 1;
        p[i % page] = (unsigned char)i;
        if (mprotect(p, page, PROT_READ) != 0 || p[i % page] != (unsigned char)i || munmap(p, page) != 0) return 1;
    }
    return 0;
}

static int perf_file(void) {
    char path[] = "/tmp/hl-perf-file-XXXXXX";
    unsigned char block[4096];
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    (void)unlink(path);
    memset(block, 0x5a, sizeof(block));
    for (unsigned i = 0; i < 4096; ++i) {
        off_t offset = (off_t)(i % 256U) * (off_t)sizeof(block);
        if (pwrite(fd, block, sizeof(block), offset) != (ssize_t)sizeof(block)) return 1;
        if (pread(fd, block, sizeof(block), offset) != (ssize_t)sizeof(block) || block[0] != 0x5a) return 1;
    }
    return close(fd) != 0;
}

static int perf_pipe(void) {
    int fds[2];
    uint64_t value = UINT64_C(0x123456789abcdef0);
    if (pipe(fds) != 0) return 1;
    for (unsigned i = 0; i < 100000; ++i)
        if (full_write(fds[1], &value, sizeof(value)) != 0 || full_read(fds[0], &value, sizeof(value)) != 0) return 1;
    return close(fds[0]) != 0 || close(fds[1]) != 0;
}

static int perf_event(void) {
    uint64_t value = 1;
    int fd = eventfd(0, EFD_CLOEXEC);
    if (fd < 0) return 1;
    for (unsigned i = 0; i < 100000; ++i)
        if (full_write(fd, &value, sizeof(value)) != 0 || full_read(fd, &value, sizeof(value)) != 0 || value != 1)
            return 1;
    return close(fd) != 0;
}

static int perf_ipc_latency(void) {
    int pair[2];
    unsigned char token = 7;
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, pair) != 0) return 1;
    pid_t child = fork();
    if (child < 0) return 1;
    if (child == 0) {
        close(pair[0]);
        for (unsigned i = 0; i < 10000; ++i)
            if (full_read(pair[1], &token, 1) != 0 || full_write(pair[1], &token, 1) != 0) _exit(1);
        _exit(0);
    }
    close(pair[1]);
    for (unsigned i = 0; i < 10000; ++i)
        if (full_write(pair[0], &token, 1) != 0 || full_read(pair[0], &token, 1) != 0) return 1;
    close(pair[0]);
    return wait_ok(child) != 0;
}

static int perf_ipc_throughput(void) {
    enum { BLOCK = 65536, BLOCKS = 1024 };

    unsigned char *block = malloc(BLOCK);
    int fds[2];
    if (block == NULL || pipe(fds) != 0) return 1;
    memset(block, 0xa5, BLOCK);
    pid_t child = fork();
    if (child < 0) return 1;
    if (child == 0) {
        close(fds[1]);
        for (unsigned i = 0; i < BLOCKS; ++i)
            if (full_read(fds[0], block, BLOCK) != 0) _exit(1);
        _exit(0);
    }
    close(fds[0]);
    for (unsigned i = 0; i < BLOCKS; ++i)
        if (full_write(fds[1], block, BLOCK) != 0) return 1;
    close(fds[1]);
    free(block);
    return wait_ok(child) != 0;
}

int main(void) {
    if (HL_PERF_OP == OP_MMAP) return perf_mmap();
    if (HL_PERF_OP == OP_FILE) return perf_file();
    if (HL_PERF_OP == OP_PIPE) return perf_pipe();
    if (HL_PERF_OP == OP_EVENT) return perf_event();
    if (HL_PERF_OP == OP_IPC_LATENCY) return perf_ipc_latency();
    if (HL_PERF_OP == OP_IPC_THROUGHPUT) return perf_ipc_throughput();
    return 2;
}
