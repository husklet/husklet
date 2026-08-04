// Exercises the eventfd WRITE path invariants that must survive the memfd-seal/fstat dispatch reorder:
//   - accumulate writes then one clearing read (non-semaphore)
//   - write with count != 8 is EINVAL and does NOT touch the counter
//   - write of value 0 is admitted (returns 8) and is a counter no-op
//   - write of UINT64_MAX is EINVAL
//   - overflow past UINT64_MAX-1 on a non-blocking eventfd is EAGAIN, counter unchanged
//   - cross-process (fork) shared counter: a child's writes are visible to the parent's read
//   - a blocking read parks until a forked writer signals, then returns the accumulated value
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    // 1) accumulate + single clearing read
    int fd = eventfd(0, EFD_NONBLOCK);
    uint64_t a = 5, b = 37;
    ssize_t w1 = write(fd, &a, 8);
    ssize_t w2 = write(fd, &b, 8);
    // 2) count != 8 -> EINVAL, counter untouched
    uint32_t small = 1;
    ssize_t wbad = write(fd, &small, 4);
    int ebad = errno;
    // 3) value 0 -> admitted, no-op
    uint64_t zero = 0;
    ssize_t wzero = write(fd, &zero, 8);
    // 4) UINT64_MAX -> EINVAL
    uint64_t max = UINT64_MAX;
    ssize_t wmax = write(fd, &max, 8);
    int emax = errno;
    uint64_t v = 0;
    ssize_t r = read(fd, &v, 8);
    printf("acc w1=%zd w2=%zd wbad=%zd(%s) wzero=%zd wmax=%zd(%s) read=%zd sum=%lu\n", w1, w2, wbad,
           strerror(ebad), wzero, wmax, strerror(emax), r, (unsigned long)v);

    // 5) overflow: seed near the cap, then a write that would exceed UINT64_MAX-1 is EAGAIN
    int of = eventfd(0, EFD_NONBLOCK);
    uint64_t near = 0xfffffffffffffffeULL;
    write(of, &near, 8);
    uint64_t one = 1;
    ssize_t wof = write(of, &one, 8);
    int eof = errno;
    printf("overflow wof=%zd errno=%s\n", wof, strerror(eof));
    close(of);

    // 6) cross-process shared counter: child writes, parent reads the sum
    int shared = eventfd(0, EFD_NONBLOCK);
    pid_t c1 = fork();
    if (c1 == 0) {
        uint64_t x = 100;
        write(shared, &x, 8);
        uint64_t y = 11;
        write(shared, &y, 8);
        _exit(0);
    }
    waitpid(c1, NULL, 0);
    uint64_t sv = 0;
    ssize_t sr = read(shared, &sv, 8);
    printf("xproc read=%zd sum=%lu\n", sr, (unsigned long)sv);
    close(shared);

    // 7) blocking read parks until a forked writer signals
    int blk = eventfd(0, 0); // blocking
    pid_t c2 = fork();
    if (c2 == 0) {
        usleep(50000);
        uint64_t z = 9;
        write(blk, &z, 8);
        _exit(0);
    }
    uint64_t bv = 0;
    ssize_t br = read(blk, &bv, 8); // blocks until child writes
    printf("wakeup read=%zd value=%lu\n", br, (unsigned long)bv);
    waitpid(c2, NULL, 0);
    close(blk);
    close(fd);
    return 0;
}
