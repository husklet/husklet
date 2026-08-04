#include "bitwise.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
    unsigned amount = (word >> 10) & 63u;
    return (word & 0x1F000000u) == 0x0A000000u &&
           ((word & (1u << 31)) || amount < 32);
}

int hl_a64_bitwise_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned right = (word >> 16) & 31u;
    unsigned left = (word >> 5) & 31u;
    unsigned destination = word & 31u;
    unsigned native_right = stolen(right) ? 17u : right;
    unsigned native_left = stolen(left) ? 16u : left;
    unsigned native_destination = destination != 31 && stolen(destination) ? 16u : destination;
    if (assembler == NULL || !valid(word)) return 0;
    if (stolen(left)) hl_a64_ldr(assembler, 16, CPU, (int)left * 8);
    if (stolen(right)) hl_a64_ldr(assembler, 17, CPU, (int)right * 8);
    hl_a64_emit32(assembler, (word & ~((31u << 16) | (31u << 5) | 31u)) |
                             (native_right << 16) | (native_left << 5) | native_destination);
    if (destination != 31 && stolen(destination))
        hl_a64_str(assembler, 16, CPU, (int)destination * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_bitwise_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_BITWISE_MAX_BYTES || !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_bitwise_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
