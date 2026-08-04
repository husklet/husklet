#include "move.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
    unsigned opcode = (word >> 29) & 3u;
    unsigned halfword = (word >> 21) & 3u;
    return (word & 0x1F800000u) == 0x12800000u && opcode != 1 &&
           ((word & (1u << 31)) || halfword < 2);
}

int hl_a64_move_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned destination = word & 31u;
    unsigned opcode = (word >> 29) & 3u;
    unsigned native_destination = stolen(destination) ? 16u : destination;
    if (assembler == NULL || !valid(word)) return 0;
    if (opcode == 3 && stolen(destination))
        hl_a64_ldr(assembler, 16, CPU, (int)destination * 8);
    hl_a64_emit32(assembler, (word & ~31u) | native_destination);
    if (stolen(destination))
        hl_a64_str(assembler, 16, CPU, (int)destination * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_move_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_MOVE_MAX_BYTES || !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_move_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
