#include "pair_arithmetic.h"

#include "stub.h"

static int valid(uint32_t word) {
    unsigned q = (word >> 30) & 1u;
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 11) & 31u;
    if ((word & UINT32_C(0x9f200400)) != UINT32_C(0x0e200400)) return 0;
    if (opcode == 0x17u) return !u && (q || size != 3u);
    if (opcode == 0x14u || opcode == 0x15u) return size != 3u;
    if (!u) return 0;
    if (opcode == 0x1au) return size < 2u && (size == 0u || q);
    if (opcode == 0x18u || opcode == 0x1eu) return (size & 1u) == 0u || q;
    return 0;
}

int hl_a64_pair_arithmetic_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !valid(word)) return 0;
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_pair_arithmetic_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_PAIR_ARITHMETIC_MAX_BYTES ||
        !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_pair_arithmetic_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
