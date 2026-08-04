#include "stub.h"

#include "entry.h"

#include <stddef.h>
#include <string.h>

enum {
    CPU = 28,
};

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
#define OFFSET_EXECUTED ((int)offsetof(hl_native_aarch64_cpu, executed))

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
    hl_a64_emit32(assembler, 0xAA0003FCu); /* mov x28,x0 */
    hl_a64_ldr(assembler, 0, 0, 0);
}

static void spill(hl_a64_assembler *assembler) {
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

void hl_a64_stub_exit_register(hl_a64_assembler *assembler, uint32_t kind, int target) {
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
    hl_a64_ldr(assembler, 16, CPU, OFFSET_BUDGET);
    hl_a64_emit32(assembler, UINT32_C(0xd53b4211)); /* mrs x17,nzcv */
    guard->compare = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0xf100021f)); /* cmp x16,#count */
    guard->budget_branch = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0x54000003)); /* b.lo budget */
    hl_a64_emit32(assembler, UINT32_C(0xd51b4211)); /* msr nzcv,x17 */
    guard->subtract = (uint32_t *)assembler->cursor;
    hl_a64_subi(assembler, 16, 16, 0);
    hl_a64_str(assembler, 16, CPU, OFFSET_BUDGET);
    hl_a64_ldr(assembler, 16, CPU, OFFSET_EXECUTED);
    guard->add = (uint32_t *)assembler->cursor;
    hl_a64_addi(assembler, 16, 16, 0);
    hl_a64_str(assembler, 16, CPU, OFFSET_EXECUTED);
}

void hl_a64_stub_budget_finish(hl_a64_assembler *assembler, hl_a64_budget_guard *guard, uint32_t count) {
    *guard->compare |= count << 10;
    *guard->subtract |= count << 10;
    *guard->add |= count << 10;
    patch_condition(guard->interrupt_branch, assembler->cursor);
    patch_condition(guard->token_interrupt_branch, assembler->cursor);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_INTERRUPT, guard->pc);
    patch_condition(guard->budget_branch, assembler->cursor);
    hl_a64_emit32(assembler, UINT32_C(0xd51b4211)); /* msr nzcv,x17 */
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
