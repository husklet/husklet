// EFD_SEMAPHORE changes the read semantics of the shared counter: each read decrements by one
// and returns 1, the counter is exhausted after N reads, a write adds to the residue, and a
// zero-value write is a no-op while an 8-byte alignment violation is EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    int fd = eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK);
    uint64_t three = 3, v = 0;
    ssize_t w = write(fd, &three, 8);
    ssize_t r1 = read(fd, &v, 8);
    uint64_t v1 = v;
    (void)!read(fd, &v, 8);
    uint64_t v2 = v;
    (void)!read(fd, &v, 8);
    uint64_t v3 = v;
    ssize_t r4 = read(fd, &v, 8);
    int e4 = (r4 == -1) ? errno : 0;
    uint64_t zero = 0;
    ssize_t wz = write(fd, &zero, 8);
    ssize_t r5 = read(fd, &v, 8);
    int e5 = (r5 == -1) ? errno : 0;
    ssize_t shortw = write(fd, &three, 4);
    int esw = (shortw == -1) ? errno : 0;
    ssize_t shortr = read(fd, &v, 4);
    int esr = (shortr == -1) ? errno : 0;
    uint64_t max = 0xffffffffffffffffULL;
    ssize_t wmax = write(fd, &max, 8);
    int ewmax = (wmax == -1) ? errno : 0;
    printf("w=%zd r1=%zd v1=%llu v2=%llu v3=%llu r4=%zd e4=%d wz=%zd r5=%zd e5=%d shortw=%zd esw=%d shortr=%zd esr=%d wmax=%zd ewmax=%d\n",
           w, r1, (unsigned long long)v1, (unsigned long long)v2, (unsigned long long)v3,
           r4, e4, wz, r5, e5, shortw, esw, shortr, esr, wmax, ewmax);
    return 0;
}
