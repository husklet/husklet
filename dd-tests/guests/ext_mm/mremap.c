// mremap(2) (Linux): grow an anonymous mapping in place / with MREMAP_MAYMOVE, preserving contents.
// No portable POSIX form (macOS lacks mremap) -> Linux-only, native oracle.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    size_t old = ps * 2, big = ps * 16;
    char *m = mmap(NULL, old, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    memset(m, 0xA5, old);
    char *m2 = mremap(m, old, big, MREMAP_MAYMOVE);
    int grown = m2 != MAP_FAILED;
    // old bytes preserved after the (possibly relocated) grow
    int preserved = grown && (unsigned char)m2[0] == 0xA5 && (unsigned char)m2[old - 1] == 0xA5;
    // newly-added tail is zero-filled and writable
    int tail_zero = grown && m2[old] == 0;
    if (grown) { memset(m2 + old, 0x5A, big - old); }
    int tail_wr = grown && (unsigned char)m2[big - 1] == 0x5A;
    // shrink back
    char *m3 = grown ? mremap(m2, big, old, 0) : MAP_FAILED;
    int shrunk = m3 != MAP_FAILED;
    if (shrunk) munmap(m3, old); else if (grown) munmap(m2, big);
    printf("mremap grown=%d preserved=%d tail_zero=%d tail_wr=%d shrunk=%d\n",
           grown, preserved, tail_zero, tail_wr, shrunk);
    return 0;
}
