// TASK #251 regression: aarch64 PC-relative literal-load family rewrite.
//
// A literal load (LDR/LDRSW/PRFM literal, LDR-literal SIMD) reads a constant from an address computed
// relative to the GUEST PC. hl translates each block to a DIFFERENT host address, so a verbatim emit would
// read from the wrong (host-PC-relative) location. The GPR and SIMD LDR-literal forms were already
// rewritten to materialize the guest-absolute literal address; LDRSW (literal) (opc=10, V=0, top byte
// 0x98, the sign-extending 32->64 word load compilers emit for switch/jump tables) was MISSING from that
// rewrite path, and PRFM (literal) (0xD8) fell through to the verbatim emit. This guest exercises the WHOLE
// family so a wrong/missing rewrite shows as a diff vs the native aarch64 oracle:
//   0x18 LDR-lit W, 0x58 LDR-lit X, 0x98 LDRSW-lit (sign-extend), 0x1C/0x5C/0x9C LDR-lit SIMD S/D/Q,
//   0xD8 PRFM-lit (a prefetch hint; must not fault or corrupt state).
//
// The "works by accident" hazard only bites when the host arena places the block far from the guest
// literal (a distant arena or a WARM pcache load): a naive host-PC-relative read then lands on the wrong
// bytes. Output is deterministic and diffed byte-exact vs native; a pcache-warm variant reruns the same
// guest with the persistent cache enabled so the 2nd (warm) run must resolve identically.
#include <stdio.h>
#include <stdint.h>

int main(void) {
    int64_t  sw_neg = 0, sw_pos = 0;
    uint32_t w32 = 0, sbits = 0;
    uint64_t x64 = 0, dbits = 0, q_lo = 0, q_hi = 0;

    // One volatile asm block with its own literal pool. `b 1f` jumps over the pool; every load references
    // its pool entry PC-relative (backward), exactly the encoding hl must rewrite to the guest address.
    __asm__ volatile(
        "b      1f\n"
        ".p2align 4\n"
        "5:  .word  0x80000001\n"          // LDRSW -> 0xFFFFFFFF80000001 (negative: proves sign-extend)
        "6:  .word  0x7FFFFFFF\n"          // LDRSW -> 0x000000007FFFFFFF ; LDR W -> 0x7FFFFFFF
        ".p2align 3\n"
        "7:  .quad  0x0123456789ABCDEF\n"  // LDR X   (0x58)
        "8:  .word  0x40490FDB\n"          // LDR S   (0x1C) float bits
        ".p2align 3\n"
        "9:  .quad  0x400921FB54442D18\n"  // LDR D   (0x5C) double bits
        ".p2align 4\n"
        "10: .quad  0x1122334455667788\n"  // LDR Q   (0x9C) low  half
        "    .quad  0x99AABBCCDDEEFF00\n"  //                high half
        "1:\n"
        "ldrsw  %0, 5b\n"                  // 0x98 LDRSW-literal (negative -> sign-extended into X)
        "ldrsw  %1, 6b\n"                  // 0x98 LDRSW-literal (positive)
        "ldr    %w2, 6b\n"                 // 0x18 LDR-literal W
        "ldr    %3, 7b\n"                  // 0x58 LDR-literal X
        "ldr    s0, 8b\n"                  // 0x1C LDR-literal SIMD S
        "fmov   %w4, s0\n"
        "ldr    d1, 9b\n"                  // 0x5C LDR-literal SIMD D
        "fmov   %5, d1\n"
        "ldr    q2, 10b\n"                 // 0x9C LDR-literal SIMD Q
        "umov   %6, v2.d[0]\n"
        "umov   %7, v2.d[1]\n"
        "prfm   pldl1keep, 5b\n"           // 0xD8 PRFM-literal (hint: must be a safe no-op)
        : "=r"(sw_neg), "=r"(sw_pos), "=r"(w32), "=r"(x64),
          "=r"(sbits), "=r"(dbits), "=r"(q_lo), "=r"(q_hi)
        :
        : "v0", "v1", "v2", "memory");

    // FNV-1a over every loaded value: any single wrong bit (wrong address / wrong sign-extend) changes it.
    uint64_t acc = 1469598103934665603ULL;
    uint64_t parts[8] = { (uint64_t)sw_neg, (uint64_t)sw_pos, w32, x64, sbits, dbits, q_lo, q_hi };
    for (int i = 0; i < 8; i++) { acc ^= parts[i]; acc *= 1099511628211ULL; }

    printf("ldrsw-lit sw_neg=%lld sw_pos=%lld w32=%08x x64=%016llx s=%08x d=%016llx q=%016llx:%016llx acc=%016llx\n",
           (long long)sw_neg, (long long)sw_pos, w32, (unsigned long long)x64,
           sbits, (unsigned long long)dbits, (unsigned long long)q_lo,
           (unsigned long long)q_hi, (unsigned long long)acc);
    return 0;
}
