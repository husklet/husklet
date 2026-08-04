#include "simd_saturating.h"

#include "stub.h"

static int valid(uint32_t word) {
    unsigned q = (word >> 30) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 11) & 31u;
    if ((word & UINT32_C(0x9f200400)) != UINT32_C(0x0e200400)) return 0;
    switch (opcode) {
        case 0x00u: /* SHADD / UHADD */
        case 0x02u: /* SRHADD / URHADD */
        case 0x04u: return size != 3u; /* SHSUB / UHSUB */
        case 0x01u: /* SQADD / UQADD */
        case 0x05u: return q || size != 3u; /* SQSUB / UQSUB */
        case 0x16u: return size == 1u || size == 2u; /* SQDMULH / SQRDMULH */
        default: return 0;
    }
}

int hl_a64_simd_saturating_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !valid(word)) return 0;
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_saturating_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_SATURATING_MAX_BYTES ||
        !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_simd_saturating_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
