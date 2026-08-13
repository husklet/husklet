#include "execution_control.h"

#include "primitives.h"
#include "../cpu.h"
#include "../encoding.h"

// MXCSR sticky exception flags <-> ARM FPSR cumulative flags. MXCSR bits 0..5 map to
// FPSR IOC, IDC, DZC, OFC, UFC, and IXC respectively.
static const int mxcsr_fpsr_bit[6] = {0, 7, 1, 2, 3, 4};

static void emit_fpsr_to_mxcsr(int destination) {
    emit32(0xD53B4420u | 22); // mrs x22, fpsr
    e_movconst(21, 0);
    e_movconst(19, 1);
    for (int index = 0; index < 6; index++) {
        e_lsr_i(20, 22, mxcsr_fpsr_bit[index], 0);
        e_rrr(A_AND, 20, 20, 19, 0, 0);
        e_rrr(A_ORR, 21, 21, 20, 0, index);
    }
    // FPCR.FZ also represents guest DAZ. ARM's IDC under FZ must therefore not
    // surface as x86 #D: the guest requested that denormal input be zeroed.
    emit32(0xD53B4400u | 22); // mrs x22, fpcr
    e_lsr_i(22, 22, 24, 0);
    e_rrr(A_AND, 22, 22, 19, 0, 0);
    e_rrr(A_BIC, 21, 21, 22, 0, 1);
    e_rrr(A_ORR, destination, destination, 21, 0, 0);
}

static void emit_mxcsr_to_fpsr(int source) {
    emit32(0xD53B4420u | 22); // mrs x22, fpsr
    e_movconst(19, 0x9f);
    e_rrr(A_BIC, 22, 22, 19, 0, 0);
    e_movconst(19, 1);
    for (int index = 0; index < 6; index++) {
        e_lsr_i(20, source, index, 0);
        e_rrr(A_AND, 20, 20, 19, 0, 0);
        e_rrr(A_ORR, 22, 22, 20, 0, mxcsr_fpsr_bit[index]);
    }
    emit32(0xD51B4420u | 22); // msr fpsr, x22
}

void emit_ldmxcsr(struct insn *instruction, uint64_t next) {
    if (!instruction->is_mem) return;
    emit_ea(instruction, next);
    emit_memory_guard(17, 4, next - (uint64_t)instruction->len, X86_SOFT_READ);
    e_load(4, 23, 17);
    e_lsr_i(16, 23, 13, 0);
    e_movconst(19, 3);
    e_rrr(A_AND, 16, 16, 19, 0, 0);
    e_movconst(19, 1);
    e_rrr(A_AND, 20, 16, 19, 0, 0);
    e_lsr_i(21, 16, 1, 0);
    e_rrr(A_ORR, 20, 21, 20, 0, 1);
    emit32(0xD53B4400u | 19); // mrs x19, fpcr
    e_movconst(21, 3u << 22);
    e_rrr(A_BIC, 19, 19, 21, 1, 0);
    e_rrr(A_ORR, 19, 19, 20, 1, 22);
    e_lsr_i(16, 23, 15, 0);
    e_lsr_i(20, 23, 6, 0);
    e_rrr(A_ORR, 16, 16, 20, 0, 0);
    e_movconst(20, 1);
    e_rrr(A_AND, 16, 16, 20, 0, 0);
    e_movconst(20, 1u << 24);
    e_rrr(A_BIC, 19, 19, 20, 1, 0);
    e_rrr(A_ORR, 19, 19, 16, 1, 24);
    emit32(0xD51B4400u | 19); // msr fpcr, x19
    emit_mxcsr_to_fpsr(23);
}

void emit_stmxcsr(struct insn *instruction, uint64_t next) {
    if (!instruction->is_mem) return;
    emit_ea(instruction, next);
    emit_memory_guard(17, 4, next - (uint64_t)instruction->len, X86_SOFT_WRITE);
    emit32(0xD53B4400u | 19); // mrs x19, fpcr
    e_lsr_i(19, 19, 22, 0);
    e_movconst(20, 3);
    e_rrr(A_AND, 19, 19, 20, 0, 0);
    e_movconst(20, 1);
    e_rrr(A_AND, 21, 19, 20, 0, 0);
    e_lsr_i(22, 19, 1, 0);
    e_rrr(A_ORR, 19, 22, 21, 0, 1);
    e_movconst(16, 0x1f80);
    e_rrr(A_ORR, 16, 16, 19, 0, 13);
    emit_fpsr_to_mxcsr(16);
    emit32(0xD53B4400u | 19); // mrs x19, fpcr
    e_lsr_i(19, 19, 24, 0);
    e_movconst(20, 1);
    e_rrr(A_AND, 19, 19, 20, 0, 0);
    e_rrr(A_ORR, 16, 16, 19, 0, 15);
    e_rrr(A_ORR, 16, 16, 19, 0, 6);
    e_store(4, 16, 17);
    if (emit_soft_memory_active()) emit_soft_store_commit(4);
}

void emit_div_zero_check(int divisor, uint64_t guest_pc, int is_signed) {
    uint32_t *nonzero = hl_x86_emit_cursor();
    emit32(0);
    e_str(divisor, 28, OFF_DIVOP);
    emit_exit_const(guest_pc, is_signed ? R_IDIV : R_DIV);
    int64_t distance = (uint8_t *)hl_x86_emit_cursor() - (uint8_t *)nonzero;
    *nonzero = 0xB5000000u | (((uint32_t)(distance / 4) & 0x7ffffu) << 5) | (uint32_t)divisor;
}

void emit_div_ovf_check(int quotient, int scratch, int width, int is_signed, uint64_t guest_pc, int signed_exit) {
    uint32_t *in_range;
    if (is_signed) {
        if (width == 4)
            e_mov_rr(scratch, quotient, 1);
        else
            e_sxt(scratch, quotient, 4);
        e_sxt(16, scratch, width);
        e_rrr(A_SUBS, 31, 16, scratch, 1, 0);
        in_range = hl_x86_emit_cursor();
        emit32(0);
    } else {
        e_lsr_i(scratch, quotient, 8 * width, 1);
        in_range = hl_x86_emit_cursor();
        emit32(0);
    }
    e_movconst(16, 0);
    e_str(16, 28, OFF_DIVOP);
    emit_exit_const(guest_pc, signed_exit ? R_IDIV : R_DIV);
    int64_t distance = (uint8_t *)hl_x86_emit_cursor() - (uint8_t *)in_range;
    if (is_signed)
        *in_range = 0x54000000u | (((uint32_t)(distance / 4) & 0x7ffffu) << 5);
    else
        *in_range = 0xB4000000u | (((uint32_t)(distance / 4) & 0x7ffffu) << 5) | (uint32_t)scratch;
}

void emit_div64_fast(uint64_t next, uint64_t guest_pc, int is_signed, int divisor) {
    e_mov_rr(23, divisor, 1);
    emit_div_zero_check(23, guest_pc, is_signed);
    uint32_t *slow_primary;
    uint32_t *slow_negative_one = 0;
    if (!is_signed) {
        slow_primary = hl_x86_emit_cursor();
        emit32(0);
        e_udiv(20, RAX, 23, 1);
        e_msub(21, 20, 23, RAX, 1);
    } else {
        e_asr_i(22, RAX, 63, 1);
        e_rrr(A_SUBS, 31, RDX, 22, 1, 0);
        slow_primary = hl_x86_emit_cursor();
        emit32(0);
        e_addi(21, 23, 1, 1);
        slow_negative_one = hl_x86_emit_cursor();
        emit32(0);
        e_sdiv(20, RAX, 23, 1);
        e_msub(21, 20, 23, RAX, 1);
    }
    e_mov_rr(RAX, 20, 1);
    e_mov_rr(RDX, 21, 1);
    uint32_t *done = hl_x86_emit_cursor();
    emit32(0);
    uint32_t *slow = hl_x86_emit_cursor();
    int64_t primary_distance = (uint8_t *)slow - (uint8_t *)slow_primary;
    if (!is_signed)
        *slow_primary = 0xB5000000u | (((uint32_t)(primary_distance / 4) & 0x7ffffu) << 5) | RDX;
    else
        *slow_primary = 0x54000000u | (((uint32_t)(primary_distance / 4) & 0x7ffffu) << 5) | 1u;
    if (slow_negative_one) {
        int64_t negative_one_distance = (uint8_t *)slow - (uint8_t *)slow_negative_one;
        *slow_negative_one = 0xB4000000u | (((uint32_t)(negative_one_distance / 4) & 0x7ffffu) << 5) | 21u;
    }
    e_str(23, 28, OFF_DIVOP);
    emit_exit_const(next, is_signed ? R_IDIV : R_DIV);
    int64_t done_distance = (uint8_t *)hl_x86_emit_cursor() - (uint8_t *)done;
    *done = 0x14000000u | ((uint32_t)(done_distance / 4) & 0x3ffffffu);
}
