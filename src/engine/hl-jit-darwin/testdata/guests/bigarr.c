// #104 repro: x86_64 large-array (>=1M elements, >8 MiB, index > 2^20) codegen fidelity.
// Differential vs the qemu oracle: any 32-bit truncation of a 64-bit index/offset, a
// disp32 sign/zero-extension slip, or a large bulk-copy length truncation in the
// x86->ARM64 lowering shows up as ONE diverging checksum line below.
//
// The address-generation idioms V8/TurboFan emits over big heap arrays are pinned with
// inline asm (so the compiler can't strength-reduce them) on x86_64; the aarch64 build
// computes the IDENTICAL value in portable C, so both engines share the golden checksums
// and the aarch64 lane is a correctness witness for the reference arch.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

#ifndef MAP_ANONYMOUS
#define MAP_ANONYMOUS MAP_ANON
#endif

#define N (2u * 1000u * 1000u) // 2,000,000 u64 -> 16 MB; index reaches ~2M (> 2^20)

static inline uint64_t pat(uint64_t i) {
    uint64_t x = i * 0x9E3779B97F4A7C15ull + 0x123456789ABCDEFull;
    x ^= x >> 29;
    x *= 0xBF58476D1CE4E5B9ull;
    x ^= x >> 32;
    return x;
}

// B) large scaled-index load  mov rdx,[base + idx*8]  with idx > 2^20.
static inline uint64_t idx_load(const uint64_t *base, uint64_t i) {
#if defined(__x86_64__)
    uint64_t v;
    __asm__ volatile("movq (%1,%2,8), %0" : "=r"(v) : "r"(base), "r"(i) : "memory");
    return v;
#else
    return base[i];
#endif
}

// I) SIGNED 32-bit index sign-extended to 64 (movsxd) then scaled -- V8's element access.
static inline uint64_t idxs_load(const uint64_t *base, int32_t si) {
#if defined(__x86_64__)
    uint64_t v;
    __asm__ volatile("movslq %2, %%rax\n\t"
                     "movq (%1,%%rax,8), %0"
                     : "=r"(v)
                     : "r"(base), "r"(si)
                     : "rax", "memory");
    return v;
#else
    return base[(int64_t)si];
#endif
}

// J) base + idx*8 + disp32 (a large positive displacement past a header).
static inline uint64_t idxd_load(const uint64_t *base, uint64_t i) {
#if defined(__x86_64__)
    uint64_t v;
    __asm__ volatile("movq 0x100000(%1,%2,8), %0" : "=r"(v) : "r"(base), "r"(i) : "memory");
    return v;
#else
    return *(const uint64_t *)((const char *)base + 0x100000 + i * 8);
#endif
}

int main(void) {
    uint64_t *a = malloc((size_t)N * sizeof(uint64_t));
    uint64_t *b = malloc((size_t)N * sizeof(uint64_t));
    if (!a || !b) { perror("malloc"); return 1; }

    // A) natural fill + sum (auto-vectorizable: movdqu/paddq on x86)
    for (uint64_t i = 0; i < N; i++) a[i] = pat(i);
    uint64_t sumA = 0;
    for (uint64_t i = 0; i < N; i++) sumA += a[i];
    printf("A fill+sum=%016llx\n", (unsigned long long)sumA);

    // B) unsigned scaled-index load, idx up to ~2M (> 2^20)
    uint64_t sumB = 0;
    for (uint64_t i = 5; i < N; i += 4099)
        sumB += idx_load(a, i) ^ (i << 20); // fold idx high bits so a 32-bit-idx slip diverges
    printf("B idxload=%016llx\n", (unsigned long long)sumB);

    // I) signed movsxd index, walking near the top of the array
    uint64_t sumI = 0;
    for (int32_t si = 1; si < (int32_t)N; si += 3571)
        sumI += idxs_load(a, si) * (uint64_t)(uint32_t)si;
    printf("I movsxd=%016llx\n", (unsigned long long)sumI);

    // J) base + idx*8 + disp32 (0x100000 header), idx stays in-bounds of the 16 MB region
    uint64_t sumJ = 0;
    for (uint64_t i = 0; i < N - (0x100000 / 8) - 1; i += 5003)
        sumJ += idxd_load(a, i) + i;
    printf("J idx+disp=%016llx\n", (unsigned long long)sumJ);

    // C) rep movsq bulk copy of the whole >16 MiB region, then checksum
    memmove(b, a, (size_t)N * sizeof(uint64_t));
    uint64_t sumC = 0;
    for (uint64_t i = 0; i < N; i++) sumC += b[i] * (i + 1);
    printf("C repmovsq=%016llx\n", (unsigned long long)sumC);

    // K) HUGE (5 GiB) demand-paged mmap so a scaled index PRODUCES a byte offset > 2^32
    //    (0x100000000): the case where a 32-bit offset truncation in the addressing lowering
    //    would actually lose bits. 5 GiB / 8 = 671,088,640 elements; index 0x20000000 (~537M)
    //    -> byte offset exactly 0x100000000 (4 GiB). Only a few pages are touched.
    const size_t HN = (5ull << 30) / sizeof(uint64_t);
    const uint64_t KBASE = 0x100000000ull / 8; // element index whose byte offset == 4 GiB
    uint64_t *h = mmap(NULL, HN * sizeof(uint64_t), PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (h == MAP_FAILED) { perror("mmap"); return 1; }
    uint64_t sumK = 0;
    // touch a handful of elements straddling the 4 GiB byte-offset boundary via scaled-index loads
    for (uint64_t i = KBASE - 3; i < KBASE + 4 && i < HN; i++) {
        h[i] = pat(i);          // store through normal codegen
        sumK += idx_load(h, i); // read back through the scaled-index idiom (offset >= 4 GiB)
    }
    munmap(h, HN * sizeof(uint64_t));
    printf("K bigoff=%016llx\n", (unsigned long long)sumK);

    free(a);
    free(b);
    return 0;
}
