#include "reverse.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
    unsigned opcode = (word >> 10) & 63u;
    unsigned wide = word >> 31;
    return (word & 0x7FE00000u) == 0x5AC00000u && opcode <= 5 &&
           (wide || opcode != 3);
}

int hl_a64_reverse_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned source = (word >> 5) & 31u;
    unsigned destination = word & 31u;
    unsigned native_source = stolen(source) ? 16u : source;
    unsigned native_destination = destination != 31 && stolen(destination) ? 17u : destination;
    if (assembler == NULL || !valid(word)) return 0;
    if (stolen(source)) hl_a64_ldr(assembler, 16, CPU, (int)source * 8);
    hl_a64_emit32(assembler, (word & ~((31u << 5) | 31u)) |
                             (native_source << 5) | native_destination);
    if (destination != 31 && stolen(destination))
        hl_a64_str(assembler, 17, CPU, (int)destination * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_reverse_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_REVERSE_MAX_BYTES || !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_reverse_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
