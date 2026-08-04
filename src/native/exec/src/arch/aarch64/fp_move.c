#include "fp_move.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
    unsigned wide = word >> 31;
    unsigned type = (word >> 22) & 3u;
    unsigned mode = (word >> 19) & 3u;
    unsigned opcode = (word >> 16) & 7u;
    if ((word & 0x5F20FC00u) != 0x1E200000u || (opcode != 6u && opcode != 7u)) return 0;
    return (!wide && type == 0u && mode == 0u) ||
           (wide && type == 1u && mode == 0u) ||
           (wide && type == 2u && mode == 1u);
}

int hl_a64_fp_move_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned opcode = (word >> 16) & 7u;
    unsigned general = opcode == 6u ? word & 31u : (word >> 5) & 31u;
    unsigned native = general != 31 && stolen(general) ? 16u : general;
    uint32_t rewritten;
    if (assembler == NULL || !valid(word)) return 0;
    if (opcode == 7u && general != 31 && stolen(general))
        hl_a64_ldr(assembler, 16, CPU, (int)general * 8);
    rewritten = opcode == 6u ? (word & ~31u) | native
                             : (word & ~(31u << 5)) | (native << 5);
    hl_a64_emit32(assembler, rewritten);
    if (opcode == 6u && general != 31 && stolen(general))
        hl_a64_str(assembler, 16, CPU, (int)general * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_fp_move_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_FP_MOVE_MAX_BYTES || !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_fp_move_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
