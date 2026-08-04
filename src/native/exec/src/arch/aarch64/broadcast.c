#include "broadcast.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int element(uint32_t word) {
    return (word & 0xbfe0fc00u) == 0x0e000400u ||
           (word & 0xffe0fc00u) == 0x5e000400u;
}

static int general(uint32_t word) {
    return (word & 0xbfe0fc00u) == 0x0e000c00u;
}

static int valid(uint32_t word) {
    unsigned immediate = (word >> 16) & 31u;
    unsigned size;
    if ((!element(word) && !general(word)) || immediate == 0) return 0;
    size = (unsigned)__builtin_ctz(immediate);
    if (size > 3) return 0;
    /* Vector 1D is reserved. Scalar copy encodings use bit 28 and write one
     * element while zeroing the remaining destination bits. */
    return size != 3 || (word & ((1u << 30) | (1u << 28))) != 0;
}

int hl_a64_broadcast_body(hl_a64_assembler *assembler, uint32_t word) {
    unsigned source = (word >> 5) & 31u;
    unsigned native = stolen(source) ? 16u : source;
    if (assembler == NULL || !valid(word)) return 0;
    if (general(word)) {
        if (stolen(source)) hl_a64_ldr(assembler, 16, CPU, (int)source * 8);
        hl_a64_emit32(assembler, (word & ~(31u << 5)) | (native << 5));
    } else {
        hl_a64_emit32(assembler, word);
    }
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_broadcast_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_BROADCAST_MAX_BYTES || !valid(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_broadcast_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
