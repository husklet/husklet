#include "stub.h"

#include "entry.h"

#include <stddef.h>
#include <string.h>

enum {
    CPU = 28,
};

static void publish_execution_identity(hl_a64_assembler *assembler, int scratch);

#define OFFSET_STACK ((int)offsetof(hl_native_aarch64_cpu, stack))
#define OFFSET_PROGRAM ((int)offsetof(hl_native_aarch64_cpu, program))
#define OFFSET_REASON ((int)offsetof(hl_native_aarch64_cpu, reason))
#define OFFSET_VECTOR ((int)offsetof(hl_native_aarch64_cpu, vectors))
#define OFFSET_FLAGS ((int)offsetof(hl_native_aarch64_cpu, flags))
#define OFFSET_FPCR ((int)offsetof(hl_native_aarch64_cpu, fpcr))
#define OFFSET_FPSR ((int)offsetof(hl_native_aarch64_cpu, fpsr))
#define OFFSET_INTERRUPT ((int)offsetof(hl_native_aarch64_cpu, interrupt))
#define OFFSET_INTERRUPT_TOKEN ((int)offsetof(hl_native_aarch64_cpu, interrupt_token))
#define OFFSET_BUDGET ((int)offsetof(hl_native_aarch64_cpu, budget))

static int stolen(int reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid_kind(uint32_t kind) {
    switch (kind) {
        case HL_NATIVE_EXIT_BRANCH:
        case HL_NATIVE_EXIT_SYSCALL:
        case HL_NATIVE_EXIT_FALLBACK:
        case HL_NATIVE_EXIT_FAULT:
        case HL_NATIVE_EXIT_INTERRUPT:
        case HL_NATIVE_EXIT_YIELD:
            return 1;
        default:
            return 0;
    }
}

void hl_a64_stub_prologue(hl_a64_assembler *assembler) {
    hl_a64_ldr(assembler, 9, 0, OFFSET_STACK);
    hl_a64_mov_sp_from(assembler, 9);
    hl_a64_ldr(assembler, 9, 0, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 9u); /* msr nzcv,x9 */
    hl_a64_ldr(assembler, 9, 0, OFFSET_FPCR);
    hl_a64_emit32(assembler, 0xD51B4400u | 9u); /* msr fpcr,x9 */
    hl_a64_ldr(assembler, 9, 0, OFFSET_FPSR);
    hl_a64_emit32(assembler, 0xD51B4420u | 9u); /* msr fpsr,x9 */
    for (int vector = 0; vector < 32; vector += 2)
        hl_a64_ldp_q(assembler, vector, vector + 1, 0, OFFSET_VECTOR + vector * 16);
    for (int reg = 1; reg <= 30; reg++)
        if (!stolen(reg)) hl_a64_ldr(assembler, reg, 0, reg * 8);
    /* Guest x30 is stolen and lives in the CPU record, so physical x30 carries
     * the remaining budget for the whole native run. */
    hl_a64_ldr(assembler, 30, 0, OFFSET_BUDGET);
    hl_a64_emit32(assembler, 0xAA0003FCu); /* mov x28,x0 */
    hl_a64_ldr(assembler, 0, 0, 0);
}

static void spill(hl_a64_assembler *assembler) {
    hl_a64_str(assembler, 30, CPU, OFFSET_BUDGET);
    for (int vector = 0; vector < 32; vector += 2)
        hl_a64_stp_q(assembler, vector, vector + 1, CPU, OFFSET_VECTOR + vector * 16);
    for (int reg = 0; reg <= 30; reg++)
        if (!stolen(reg)) hl_a64_str(assembler, reg, CPU, reg * 8);
    hl_a64_emit32(assembler, 0xD53B4200u); /* mrs x0,nzcv */
    hl_a64_str(assembler, 0, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD53B4400u); /* mrs x0,fpcr */
    hl_a64_str(assembler, 0, CPU, OFFSET_FPCR);
    hl_a64_emit32(assembler, 0xD53B4420u); /* mrs x0,fpsr */
    hl_a64_str(assembler, 0, CPU, OFFSET_FPSR);
    hl_a64_mov_from_sp(assembler, 0);
    hl_a64_str(assembler, 0, CPU, OFFSET_STACK);
    hl_a64_movr(assembler, 0, CPU);
}

void hl_a64_stub_exit(hl_a64_assembler *assembler, uint32_t kind, uint64_t pc) {
    if (assembler->diagnostics && kind == HL_NATIVE_EXIT_BRANCH)
        hl_a64_stub_publish_execution_identity(assembler);
    spill(assembler);
    hl_a64_movconst(assembler, 9, pc);
    hl_a64_str(assembler, 9, 0, OFFSET_PROGRAM);
    hl_a64_movconst(assembler, 9, kind);
    hl_a64_str(assembler, 9, 0, OFFSET_REASON);
#if defined(__aarch64__)
    hl_a64_adrp_add(assembler, 9, (uint64_t)(uintptr_t)hl_native_aarch64_return);
#else
    hl_a64_movconst(assembler, 9, 0);
#endif
    hl_a64_br(assembler, 9);
}

uint32_t *hl_a64_stub_edge_reserve(hl_a64_assembler *assembler) {
    uint32_t *span = assembler == NULL ? NULL : (uint32_t *)assembler->cursor;
    for (uint32_t index = 0; index < HL_A64_EDGE_SPAN_WORDS; index++)
        hl_a64_emit32(assembler, UINT32_C(0xd503201f));
    return span;
}

void hl_a64_stub_exit_register(hl_a64_assembler *assembler, uint32_t kind, int target) {
    if (assembler->diagnostics && kind == HL_NATIVE_EXIT_BRANCH)
        publish_execution_identity(assembler, target == 17 ? 16 : 17);
    spill(assembler);
    hl_a64_str(assembler, target, 0, OFFSET_PROGRAM);
    hl_a64_movconst(assembler, 9, kind);
    hl_a64_str(assembler, 9, 0, OFFSET_REASON);
#if defined(__aarch64__)
    hl_a64_adrp_add(assembler, 9, (uint64_t)(uintptr_t)hl_native_aarch64_return);
#else
    hl_a64_movconst(assembler, 9, 0);
#endif
    hl_a64_br(assembler, 9);
}

static void patch_condition(uint32_t *branch, const uint8_t *target) {
    uint32_t distance = (uint32_t)((target - (const uint8_t *)branch) / 4);
    *branch |= (distance & UINT32_C(0x7ffff)) << 5;
}

static void patch_branch(uint32_t *branch, const uint8_t *target) {
    uint32_t distance = (uint32_t)((target - (const uint8_t *)branch) / 4);
    *branch |= distance & UINT32_C(0x03ffffff);
}

static void publish_execution_identity(hl_a64_assembler *assembler, int scratch) {
    /* Cache execution lookup accepts any address inside the executing entry,
     * tagged in bit zero.  AArch64 instructions are four-byte aligned, so the
     * tag cannot alias an instruction address. */
    hl_a64_emit32(assembler, UINT32_C(0x10000000) | (uint32_t)scratch); /* adr scratch,. */
    hl_a64_addi(assembler, scratch, scratch, 1);
    hl_a64_str(assembler, scratch, 28,
               (int)offsetof(hl_native_aarch64_cpu, execution_identity));
}

void hl_a64_stub_publish_execution_identity(hl_a64_assembler *assembler) {
    publish_execution_identity(assembler, 17);
}

void hl_a64_stub_budget_begin(hl_a64_assembler *assembler, uint64_t pc, hl_a64_budget_guard *guard) {
    memset(guard, 0, sizeof(*guard));
    guard->pc = pc;
    hl_a64_ldr(assembler, 16, CPU, OFFSET_INTERRUPT);
    guard->interrupt_branch = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0xb5000010)); /* cbnz x16,interrupt */
    hl_a64_ldr(assembler, 16, CPU, OFFSET_INTERRUPT_TOKEN);
    guard->token_skip_branch = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0xb4000010)); /* cbz x16,no-token */
    hl_a64_emit32(assembler, UINT32_C(0xc8dffe10)); /* ldar x16,[x16] */
    guard->token_interrupt_branch = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0xb5000010)); /* cbnz x16,interrupt */
    patch_condition(guard->token_skip_branch, assembler->cursor);
    guard->subtract = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0xd10003d1)); /* sub x17,x30,#count */
    /* Borrow test, so entry admission leaves guest NZCV alone. The reject arm
     * is an unconditional branch because a block body can outrange tbnz. */
    hl_a64_emit32(assembler, UINT32_C(0xb6f80000) | (UINT32_C(2) << 5) | 17u); /* tbz x17,#63,+2 */
    guard->budget_branch = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0x14000000)); /* b budget */
    hl_a64_movr(assembler, 30, 17);
}

void hl_a64_stub_budget_finish(hl_a64_assembler *assembler, hl_a64_budget_guard *guard, uint32_t count) {
    *guard->subtract |= count << 10;
    patch_condition(guard->interrupt_branch, assembler->cursor);
    patch_condition(guard->token_interrupt_branch, assembler->cursor);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_INTERRUPT, guard->pc);
    patch_branch(guard->budget_branch, assembler->cursor);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_YIELD, guard->pc);
}

int hl_a64_stub_emit(hl_a64_assembler *assembler, uint32_t kind, uint64_t pc) {
    if (assembler == NULL || !valid_kind(kind) ||
        hl_a64_assembler_remaining(assembler) < HL_A64_STUB_MAX_BYTES)
        return 0;
    hl_a64_stub_prologue(assembler);
    hl_a64_stub_exit(assembler, kind, pc);
    return hl_a64_assembler_ok(assembler);
}
