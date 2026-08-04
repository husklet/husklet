#include "terminator.h"

#include "stub.h"

int hl_a64_terminator_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_TERMINATOR_MAX_BYTES ||
        word != 0xD4000001u)
        return 0;
    hl_a64_stub_prologue(assembler);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_SYSCALL, pc);
    return hl_a64_assembler_ok(assembler);
}
