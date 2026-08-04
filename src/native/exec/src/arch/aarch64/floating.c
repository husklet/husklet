#include "floating.h"

#include "stub.h"

static int valid(uint32_t word) {
    /* Three-source FMADD/FMSUB/FNMADD/FNMSUB and the scalar FP box whose
     * opcode field is nonzero.  The excluded zero-opcode forms are the
     * integer/fixed conversions and GPR moves handled by typed emitters. */
    if ((word & 0x5f000000u) == 0x1f000000u) return 1;
    return (word & 0x5f000000u) == 0x1e000000u && (word & 0x00200000u) != 0 &&
           (word & 0x0000fc00u) != 0;
}

int hl_a64_floating_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !valid(word)) return 0;
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_floating_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_FLOATING_MAX_BYTES || !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_floating_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
