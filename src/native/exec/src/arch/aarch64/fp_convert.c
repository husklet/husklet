#include "fp_convert.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
    unsigned type = (word >> 22) & 3u;
    unsigned rmode = (word >> 19) & 3u;
    unsigned opcode = (word >> 16) & 7u;
    /* Scalar integer conversion (not the fixed-point encoding). Half and
     * reserved types remain behind feature-aware owners. FMOV is owned by
     * fp_move.c. */
    if ((word & 0x5f20fc00u) != 0x1e200000u || type > 1u || opcode > 5u)
        return 0;
    if (opcode == 2u || opcode == 3u || opcode == 4u || opcode == 5u)
        return rmode == 0u;
    return 1;
}

int hl_a64_fp_convert_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned opcode = (word >> 16) & 7u;
    int from_general = opcode == 2u || opcode == 3u;
    unsigned general = from_general ? (word >> 5) & 31u : word & 31u;
    unsigned native = general != 31u && stolen(general) ? 16u : general;
    uint32_t rewritten;
    if (assembler == NULL || !valid(word)) return 0;
    if (from_general && general != 31u && stolen(general))
        hl_a64_ldr(assembler, 16, CPU, (int)general * 8);
    rewritten = from_general ? (word & ~(31u << 5)) | (native << 5)
                             : (word & ~31u) | native;
    hl_a64_emit32(assembler, rewritten);
    if (!from_general && general != 31u && stolen(general))
        hl_a64_str(assembler, 16, CPU, (int)general * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_fp_convert_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_FP_CONVERT_MAX_BYTES || !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_fp_convert_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
