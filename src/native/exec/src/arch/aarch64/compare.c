#include "compare.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) { return (word & 0x1FE00410u) == 0x1A400000u; }

int hl_a64_compare_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned right = (word >> 16) & 31u;
    unsigned left = (word >> 5) & 31u;
    int immediate = (word & (1u << 11)) != 0;
    uint32_t native;
    if (assembler == NULL || !valid(word)) return 0;
    if (stolen(left)) hl_a64_ldr(assembler, 16, CPU, (int)left * 8);
    if (!immediate && stolen(right)) hl_a64_ldr(assembler, 17, CPU, (int)right * 8);
    native = (word & ~(31u << 5)) | ((stolen(left) ? 16u : left) << 5);
    if (!immediate)
        native = (native & ~(31u << 16)) | ((stolen(right) ? 17u : right) << 16);
    hl_a64_emit32(assembler, native);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_compare_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_COMPARE_MAX_BYTES || !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_compare_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
