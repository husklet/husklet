#include "multiply.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int high(uint32_t word) {
    unsigned operation = (word >> 21) & 7u;
    return (word & 0x1F000000u) == 0x1B000000u && (word >> 31) != 0 &&
           (operation == 2u || operation == 6u) && (word & (1u << 15)) == 0 &&
           ((word >> 10) & 31u) == 31u;
}

static int valid(uint32_t word) {
    unsigned operation;
    if ((word & 0x1F000000u) != 0x1B000000u) return 0;
    operation = (word >> 21) & 7u;
    if (operation == 0u) return 1;
    if ((word >> 31) == 0) return 0;
    return operation == 1u || operation == 5u || high(word);
}

int hl_a64_multiply_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned right = (word >> 16) & 31u;
    unsigned accumulator = (word >> 10) & 31u;
    unsigned left = (word >> 5) & 31u;
    unsigned destination = word & 31u;
    unsigned native_destination = destination != 31 && (stolen(destination) || destination == 9) ? 16u : destination;
    uint32_t native;
    int saved_nine = stolen(accumulator);
    if (assembler == NULL || !valid(word)) return 0;
    if (stolen(left)) hl_a64_ldr(assembler, 16, CPU, (int)left * 8);
    if (stolen(right)) hl_a64_ldr(assembler, 17, CPU, (int)right * 8);
    if (saved_nine) {
        hl_a64_str(assembler, 9, CPU, 9 * 8);
        hl_a64_ldr(assembler, 9, CPU, (int)accumulator * 8);
    }
    native = word & ~((31u << 16) | (31u << 10) | (31u << 5) | 31u);
    native |= (stolen(right) ? 17u : right) << 16;
    native |= (stolen(accumulator) ? 9u : accumulator) << 10;
    native |= (stolen(left) ? 16u : left) << 5;
    native |= native_destination;
    hl_a64_emit32(assembler, native);
    if (saved_nine) hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    if (destination == 9 && native_destination == 16)
        hl_a64_movr(assembler, 9, 16);
    else if (destination != 31 && stolen(destination))
        hl_a64_str(assembler, 16, CPU, (int)destination * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_multiply_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_MULTIPLY_MAX_BYTES || !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_multiply_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
