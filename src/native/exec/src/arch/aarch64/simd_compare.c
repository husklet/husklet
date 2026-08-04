#include "simd_compare.h"

#include "stub.h"

static int valid(uint32_t word) {
    unsigned opcode = (word >> 11) & 31u;
    unsigned size = (word >> 22) & 3u;
    unsigned wide = (word >> 30) & 1u;
    return (word & 0x9F200400u) == 0x0E200400u &&
           (opcode == 0x06u || opcode == 0x07u || opcode == 0x11u) &&
           (size != 3u || wide);
}

int hl_a64_simd_compare_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !valid(word)) return 0;
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_compare_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_COMPARE_MAX_BYTES ||
        !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_simd_compare_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
