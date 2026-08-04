#include "simd_permute.h"

#include "stub.h"

static int valid(uint32_t word) {
    if ((word & UINT32_C(0xbfe08400)) == UINT32_C(0x2e000000)) {
        unsigned position = (word >> 11) & 15u;
        return ((word >> 30) & 1u) != 0 || position < 8u;
    }
    if ((word & UINT32_C(0xbf208c00)) == UINT32_C(0x0e000000)) return 1;
    if ((word & UINT32_C(0xbf208c00)) == UINT32_C(0x0e000800)) {
        unsigned q = (word >> 30) & 1u;
        unsigned size = (word >> 22) & 3u;
        unsigned opcode = (word >> 12) & 7u;
        return (q != 0 || size != 3u) && opcode != 0u && opcode != 4u;
    }
    return 0;
}

int hl_a64_simd_permute_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !valid(word)) return 0;
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_permute_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_PERMUTE_MAX_BYTES ||
        !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_simd_permute_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
