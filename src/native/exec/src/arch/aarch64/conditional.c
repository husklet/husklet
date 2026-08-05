#include "conditional.h"

#include "stub.h"

#define CPU 28

typedef enum predicate_kind {
    PREDICATE_CONDITION,
    PREDICATE_COMPARE,
    PREDICATE_TEST,
} predicate_kind;

static int64_t sign_extend(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1);
    return (int64_t)((value ^ sign) - sign);
}

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static void source(hl_a64_assembler *assembler, unsigned reg) {
    if (stolen(reg))
        hl_a64_ldr(assembler, 16, CPU, (int)reg * 8);
    else
        hl_a64_movr(assembler, 16, (int)reg);
}

static void patch(uint32_t *instruction, const uint8_t *target, uint32_t word,
                  predicate_kind kind) {
    uint32_t distance = (uint32_t)((target - (const uint8_t *)instruction) / 4);
    if (kind == PREDICATE_CONDITION && (word & 15u) >= 14)
        *instruction = 0x14000000u | (distance & 0x03FFFFFFu);
    else if (kind == PREDICATE_CONDITION)
        *instruction = 0x54000000u | ((distance & 0x7FFFFu) << 5) | (word & 15u);
    else if (kind == PREDICATE_COMPARE)
        *instruction = (word & 0xFF000000u) | ((distance & 0x7FFFFu) << 5) | 16u;
    else
        *instruction = (word & 0xFFF80000u) | ((distance & 0x3FFFu) << 5) | 16u;
}

static int decode(uint32_t word, predicate_kind *kind, int64_t *displacement) {
    if ((word & 0xFF000010u) == 0x54000000u) {
        *kind = PREDICATE_CONDITION;
        *displacement = sign_extend((word >> 5) & 0x7FFFFu, 19) * 4;
    } else if ((word & 0x7E000000u) == 0x34000000u) {
        *kind = PREDICATE_COMPARE;
        *displacement = sign_extend((word >> 5) & 0x7FFFFu, 19) * 4;
    } else if ((word & 0x7E000000u) == 0x36000000u) {
        *kind = PREDICATE_TEST;
        *displacement = sign_extend((word >> 5) & 0x3FFFu, 14) * 4;
    } else return 0;
    return 1;
}

int hl_a64_conditional_chain(hl_a64_assembler *assembler, uint32_t word, uint64_t pc,
                             uint32_t **fall_patch, uint64_t *fall_target,
                             uint32_t **taken_patch, uint64_t *taken_target) {
    predicate_kind kind;
    int64_t displacement;
    uint32_t *branch;
    if (assembler == NULL || !decode(word, &kind, &displacement)) return 0;

    if (kind != PREDICATE_CONDITION) source(assembler, word & 31u);
    branch = (uint32_t *)assembler->cursor;
    if (kind == PREDICATE_CONDITION && (word & 15u) >= 14)
        hl_a64_emit32(assembler, 0x14000000u);
    else
        hl_a64_emit32(assembler, 0);
    if (fall_patch != NULL) *fall_patch = (uint32_t *)assembler->cursor;
    if (fall_target != NULL) *fall_target = pc + 4;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    patch(branch, assembler->cursor, word, kind);
    if (taken_patch != NULL) *taken_patch = (uint32_t *)assembler->cursor;
    if (taken_target != NULL) *taken_target = pc + (uint64_t)displacement;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + (uint64_t)displacement);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_conditional_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    return hl_a64_conditional_chain(assembler, word, pc, NULL, NULL, NULL, NULL);
}

int hl_a64_conditional_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    predicate_kind kind;
    int64_t displacement;
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_CONDITIONAL_MAX_BYTES ||
        !decode(word, &kind, &displacement)) return 0;
    (void)kind;
    (void)displacement;
    hl_a64_stub_prologue(assembler);
    return hl_a64_conditional_body(assembler, word, pc);
}
