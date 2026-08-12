// Regression for #209: a partial munmap must unmap only [addr,addr+len) and KEEP the remainder of the
// tracked mapping alive + tracked. The engine records every guest mmap in a registry (gmap); the bug
// dropped the WHOLE entry (gmap_del) on a partial unmap, so the still-mapped survivor lost its tracking
// (leaked at execve teardown; mis-sized by the mremap in-place-grow path). The fix splits/shrinks the
// entry (gmap_split_unmap) so head-unmap keeps the tail, tail-unmap keeps the head, and middle-unmap
// splits into two. This asserts the Linux-visible contract that is byte-exact across both guest arches:
//   * a partial (head / middle) munmap returns 0 (never EINVAL),
//   * every SURVIVING sub-region stays mapped and readable with its sentinel intact (the split neither
//     over-releases the survivor nor corrupts it), and
//   * each survivor is INDEPENDENTLY unmappable afterwards (returns 0) — impossible if its tracking was
//     lost or its bounds were mangled by the split.
// NOTE (intentionally not asserted): whether the *unmapped* region faults on a later access is NOT
// byte-exact here — the x86_64 guest uses 4 KiB pages on a 16 KiB macOS host page, so the engine can
// only physically release whole host pages and a partial-edge head stays mapped (Linux-legal: a partial
// unmap never faults). Sizes below are 64 KiB units (a multiple of both guest page sizes) so the survivor
// bookkeeping is exercised identically on aarch64 and x86_64.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define P (64UL * 1024) // 64 KiB test unit

int main(void) {
    // ---- HEAD unmap: unmapping the first unit must keep the tail [P,4P) mapped + tracked ----
    {
        unsigned long n = 4 * P;
        char *b = mmap(0, n, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (b == MAP_FAILED) { printf("munmap_partial mmap_fail\n"); return 1; }
        char *tail = b + P;
        tail[0] = 0x11;  // sentinel at the start of the survivor
        b[n - 1] = 0x5a; // sentinel at the end of the survivor
        int unmap = munmap(b, P);                        // partial: unmap only the HEAD unit
        int tail_a = (unsigned char)tail[0];             // survivor still readable, intact
        int tail_b = (unsigned char)b[n - 1];
        int free = munmap(tail, 3 * P);                  // survivor is independently unmappable
        printf("munmap_partial head: unmap=%d tail_a=%d tail_b=%d free=%d\n", unmap, tail_a, tail_b, free);
    }
    // ---- MIDDLE unmap: unmapping the interior [P,3P) must split into head + tail survivors ----
    {
        unsigned long n = 4 * P;
        char *b = mmap(0, n, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (b == MAP_FAILED) { printf("munmap_partial mmap_fail2\n"); return 1; }
        b[0] = 0x33;     // sentinel in the head survivor
        b[n - 1] = 0x44; // sentinel in the tail survivor
        int unmap = munmap(b + P, 2 * P);                // partial: unmap the two interior units
        int head = (unsigned char)b[0];                  // both survivors still readable, intact
        int tail = (unsigned char)b[n - 1];
        int free_h = munmap(b, P);                       // head survivor independently unmappable
        int free_t = munmap(b + 3 * P, P);               // tail survivor independently unmappable
        printf("munmap_partial middle: unmap=%d head=%d tail=%d free_h=%d free_t=%d\n", unmap, head, tail,
               free_h, free_t);
    }
    return 0;
}
