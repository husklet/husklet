#include "divide.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
    uint32_t form = word & 0x7FE0FC00u;
    return form == 0x1AC00800u || form == 0x1AC00C00u;
}

int hl_a64_divide_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned divisor = (word >> 16) & 31u;
    unsigned dividend = (word >> 5) & 31u;
    unsigned destination = word & 31u;
    unsigned native_divisor = stolen(divisor) ? 17u : divisor;
    unsigned native_dividend = stolen(dividend) ? 16u : dividend;
    unsigned native_destination = destination != 31 && stolen(destination) ? 16u : destination;
    if (assembler == NULL || !valid(word)) return 0;
    if (stolen(dividend)) hl_a64_ldr(assembler, 16, CPU, (int)dividend * 8);
    if (stolen(divisor)) hl_a64_ldr(assembler, 17, CPU, (int)divisor * 8);
    hl_a64_emit32(assembler, (word & ~((31u << 16) | (31u << 5) | 31u)) |
                             (native_divisor << 16) | (native_dividend << 5) | native_destination);
    if (destination != 31 && stolen(destination))
        hl_a64_str(assembler, 16, CPU, (int)destination * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_divide_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_DIVIDE_MAX_BYTES || !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_divide_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
