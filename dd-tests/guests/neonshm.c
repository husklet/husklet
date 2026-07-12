// NEON multi-vector structure store (ST1/LD1 {v0-v3},[Xn],#imm, post-indexed) streamed into a HIGH-address
// MAP_SHARED memfd buffer, inside a hot call/loop shape that forces block chaining + the block-prologue
// red-zone spill. Models pixman's ARM-NEON blit into a wl_shm buffer. Linux-specific (memfd_create);
// diffed against a native aarch64-Linux oracle. Must run with no host fault and correct stored bytes.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

// One NEON "blit": store 4 vectors (64B) via ST1 {v0.16b-v3.16b},[dst],#64 and read them back with the
// matching LD1, streaming across `n` bytes. `pat` seeds the vectors. Returns a running checksum. Kept in
// its own function (a call boundary => a guest bl/ret => block chaining into the hot NEON block).
static uint64_t neon_blit(uint8_t *dst, const uint8_t *src, size_t n, uint64_t pat) {
    uint64_t sum = 0;
    // seed v0..v3 from pat (DUP general -> vector), independent of src
    __asm__ volatile(
        "dup v0.2d, %0\n"
        "dup v1.2d, %0\n"
        "dup v2.2d, %0\n"
        "dup v3.2d, %0\n" ::"r"(pat)
        : "v0", "v1", "v2", "v3");
    uint8_t *d = dst;
    const uint8_t *s = src;
    size_t i = 0;
    // hot inner loop: 64B per iteration, the 4-register post-indexed structure store + load
    for (; i + 64 <= n; i += 64) {
        __asm__ volatile(
            "ld1 {v4.16b-v7.16b}, [%1], #64\n"    // read source (post-index #64)
            "eor v0.16b, v0.16b, v4.16b\n"
            "eor v1.16b, v1.16b, v5.16b\n"
            "eor v2.16b, v2.16b, v6.16b\n"
            "eor v3.16b, v3.16b, v7.16b\n"
            "st1 {v0.16b-v3.16b}, [%0], #64\n"    // THE store: 4-reg struct store, post-index #64, into shm
            : "+r"(d), "+r"(s)
            :
            : "v0", "v1", "v2", "v3", "v4", "v5", "v6", "v7", "memory");
    }
    // tail with the 8-byte-lane / #32 immediate form (ST1 {v0.8b-v3.8b},[Xn],#32) exactly as pixman emits
    for (; i + 32 <= n; i += 32) {
        __asm__ volatile(
            "st1 {v0.8b-v3.8b}, [%0], #32\n"
            : "+r"(d)
            :
            : "memory");
    }
    // fold a SPARSE checksum (one byte per 64 B) so the store results are observed without an O(n) scan
    for (size_t k = 0; k < n; k += 64) sum += dst[k];
    return sum;
}

int main(void) {
    // Two sizes the bug was seen at: 188 KB and 6.1 MB.
    const size_t sizes[2] = {188 * 1024, 6100 * 1024};
    uint64_t total = 0;
    for (int si = 0; si < 2; si++) {
        size_t n = sizes[si];
        int fd = memfd_create("wl_shm", 0);
        if (fd < 0) { perror("memfd_create"); return 1; }
        if (ftruncate(fd, (off_t)n) < 0) { perror("ftruncate"); return 1; }
        // MAP_SHARED, kernel-chosen (high) address — the wl_shm buffer shape.
        uint8_t *buf = mmap(NULL, n, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (buf == MAP_FAILED) { perror("mmap"); return 1; }
        // a private source of the same size
        uint8_t *src = mmap(NULL, n, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
        if (src == MAP_FAILED) { perror("mmap src"); return 1; }
        for (size_t k = 0; k < n; k += 64) src[k] = (uint8_t)(k * 31 + si);
        // repeat the blit enough that the inner NEON loop goes hot (tier-2 promotion) and the call
        // chains repeatedly, but light enough for the native oracle to finish quickly.
        for (int rep = 0; rep < 16; rep++)
            total += neon_blit(buf, src, n, 0x0102030405060708ull + rep);
        munmap(buf, n);
        munmap(src, n);
        close(fd);
    }
    printf("neonshm ok=1 total=%llu\n", (unsigned long long)total);
    return 0;
}
