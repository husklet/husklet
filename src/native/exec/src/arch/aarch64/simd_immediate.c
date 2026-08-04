#include "simd_immediate.h"

#include "stub.h"

static int valid(uint32_t word) {
    unsigned operation, cmode, o2, wide, selector, low;
    if ((word & UINT32_C(0x9ff80400)) != UINT32_C(0x0f000400)) return 0;
    operation = (word >> 29) & 1u;
    cmode = (word >> 12) & 15u;
    o2 = (word >> 11) & 1u;
    wide = (word >> 30) & 1u;
    selector = (cmode >> 1) & 7u;
    low = cmode & 1u;
    if (selector != 7u) return o2 == 0u;
    if ((low == 0u || operation != 0u) && o2 != 0u) return 0;
    if (low != 0u && operation != 0u) return wide != 0u;
    return 1;
}

int hl_a64_simd_immediate_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !valid(word)) return 0;
    /* The guest and native ISA are identical. Retaining the complete encoding
     * preserves cmode expansion, Q-width zeroing, ORR/BIC read-modify behavior,
     * and FP immediate bits without reconstructing them in engine code. */
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_immediate_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_IMMEDIATE_MAX_BYTES ||
        !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_simd_immediate_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
