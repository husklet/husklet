#include "simd.h"

#include "primitives.h"
#include "../cpu.h"
#include "../encoding.h"

static uint32_t register_field(int register_index, unsigned shift) {
    return (uint32_t)register_index << shift;
}

// Gather the 16 byte-MSBs of vm into the low 16 bits of GPR `dst`.
static void emit_pmovmask(int vm, int dst) {
    e_vshr_imm(17, vm, 8, 7, 0);
    emit32(0x6F001400u | (25u << 16) | (17 << 5) | 17);
    emit32(0x6F001400u | (50u << 16) | (17 << 5) | 17);
    emit32(0x6F001400u | (100u << 16) | (17 << 5) | 17);
    emit32(0x0E003C00u | (1u << 16) | (17 << 5) | 16);
    emit32(0x0E003C00u | (17u << 16) | (17u << 5) | (uint32_t)dst);
    e_rrr(A_ORR, dst, 16, dst, 0, 8);
}

// PCMPISTRI implicit-length EQUAL-EACH byte, the SSE4.2 strcmp hot path.
void hl_x86_emit_pcmpistri_eqeach_byte(int av, int bv, int imm) {
    int neg = imm & 0x10, masked = imm & 0x20, msb = imm & 0x40;
    emit32(0x6E208C00u | register_field(bv, 16) | register_field(av, 5) | 18u);
    emit32(0x4E209800u | register_field(av, 5) | 19u);
    emit32(0x4E209800u | register_field(bv, 5) | 21u);
    emit_pmovmask(18, 19);
    emit_pmovmask(19, 21);
    emit_pmovmask(21, 24);

    e_movz(16, 1, 1);
    e_rrr(A_ORR, 17, 21, 16, 0, 0);
    e_rbit(17, 17, 0);
    e_clz(22, 17, 0);
    e_rrr(A_ORR, 17, 24, 16, 0, 0);
    e_rbit(17, 17, 0);
    e_clz(23, 17, 0);

    e_movz(16, 1, 0);
    e_shv(S_LSLV, 25, 16, 22, 0);
    e_subi(25, 25, 1, 0);
    e_shv(S_LSLV, 26, 16, 23, 0);
    e_subi(26, 26, 1, 0);
    e_rrr(A_AND, 17, 19, 25, 0, 0);
    e_rrr(A_AND, 17, 17, 26, 0, 0);
    e_rrr(A_ORR, 16, 25, 26, 0, 0);
    e_rrr(A_ORN, 17, 17, 16, 0, 0);
    e_uxt(17, 17, 2);

    if (neg) {
        if (masked)
            e_rrr(A_EOR, 17, 17, 26, 0, 0);
        else {
            e_movz(16, 0xFFFF, 0);
            e_rrr(A_EOR, 17, 17, 16, 0, 0);
        }
        e_uxt(17, 17, 2);
    }

    if (!msb) {
        e_movz(16, 1, 1);
        e_rrr(A_ORR, 16, 17, 16, 0, 0);
        e_rbit(16, 16, 0);
        e_clz(1, 16, 0);
    } else {
        e_clz(16, 17, 0);
        e_movz(25, 31, 0);
        e_rrr(A_SUB, 16, 25, 16, 0, 0);
        e_subi_s(31, 17, 0, 0);
        e_movz(25, 16, 0);
        e_csel(1, 25, 16, 0, 0);
    }

    e_movz(20, 0, 0);
    e_subi_s(31, 22, 16, 0);
    e_cset(16, 3, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 31);
    e_subi_s(31, 23, 16, 0);
    e_cset(16, 3, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 30);
    e_subi_s(31, 17, 0, 0);
    e_cset(16, 0, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 29);
    e_movz(25, 1, 0);
    e_rrr(A_AND, 16, 17, 25, 0, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 28);
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20);
    e_movz(16, 1, 0);
    e_str(16, 28, OFF_PF);
    e_str(31, 28, OFF_AF);
}

// SSE2 variable-count packed shift. Clamp the scalar x86 count before DUP and
// preserve the deferred guest flags across the host-side comparison.
void e_sse_var_shift(int vd, int vn, int vs, int esize, int left, int arith) {
    uint32_t sz = esize == 16 ? 1u : esize == 32 ? 2u : 3u;
    uint32_t imm5 = esize == 16 ? 2u : esize == 32 ? 4u : 8u;
    emit32(0x4E083C00u | register_field(vs, 5) | 16u);
    e_movconst(19, (uint64_t)(unsigned)esize);
    emit32(0xD53B4200u | 22);
    e_rrr(A_SUBS, 31, 16, 19, 1, 0);
    e_csel(16, 19, 16, 8, 1);
    emit32(0xD51B4200u | 22);
    if (!left) e_rrr(A_SUB, 16, 31, 16, 1, 0);
    emit32(0x4E000C00u | (imm5 << 16) | (16 << 5) | 17);
    uint32_t shl = (arith ? 0x4E204400u : 0x6E204400u) | (sz << 22);
    emit32(shl | (17u << 16) | register_field(vn, 5) | (uint32_t)vd);
}

void hl_x86_emit_dnan_pre(int vd, int source, int two_inputs, int double_precision) {
    uint32_t equal = double_precision ? 0x4E60E400u : 0x4E20E400u;
    unsigned sign_shift = double_precision ? 127u : 63u;
    if (two_inputs) {
        emit32(equal | register_field(vd, 16) | register_field(vd, 5) | 20u);
        emit32(equal | register_field(source, 16) | register_field(source, 5) | 21u);
        e_v3(0x4E201C00u, 20, 20, 21);
    } else {
        emit32(equal | register_field(source, 16) | register_field(source, 5) | 20u);
    }
    emit32(0x4F005400u | (sign_shift << 16) | (20 << 5) | 20);
}

void hl_x86_emit_dnan_post(int vd, int double_precision, int packed) {
    uint32_t equal = double_precision ? 0x4E60E400u : 0x4E20E400u;
    if (packed) {
        emit32(equal | register_field(vd, 16) | register_field(vd, 5) | 21u);
        e_v3(0x4E601C00u, 20, 20, 21);
        e_v3(0x4EA01C00u, vd, vd, 20);
        return;
    }

    emit32(0xD53B4200u | 22);
    emit32((double_precision ? 0x1E602000u : 0x1E202000u) | register_field(vd, 16) | register_field(vd, 5));
    uint32_t *not_nan = hl_x86_emit_cursor();
    emit32(0);
    emit32(equal | register_field(vd, 16) | register_field(vd, 5) | 21u);
    e_v3(0x4E601C00u, 20, 20, 21);
    e_v3(0x4EA01C00u, vd, vd, 20);
    uint8_t *complete = (uint8_t *)hl_x86_emit_cursor();
    *not_nan = 0x54000000u | ((uint32_t)(((complete - (uint8_t *)not_nan) / 4) & 0x7FFFF) << 5) | 7;
    emit32(0xD51B4200u | 22);
}

void hl_x86_emit_nan_input_gate(int vd, int source, int double_precision, uint64_t guest_pc) {
    uint32_t equal = double_precision ? 0x4E60E400u : 0x4E20E400u;
    emit32(equal | register_field(vd, 16) | register_field(vd, 5) | 24u);
    emit32(equal | register_field(source, 16) | register_field(source, 5) | 25u);
    e_v3(0x4E201C00u, 24, 24, 25);
    e_ext(25, 24, 24, 8);
    e_v3(0x4E201C00u, 24, 24, 25);
    e_fmov_from_d(16, 24);
    e_rrr(A_ORN, 16, 31, 16, 1, 0);
    uint32_t *clean = hl_x86_emit_cursor();
    emit32(0);
    emit_exit_const(guest_pc, R_SSE3B);
    uint8_t *complete = (uint8_t *)hl_x86_emit_cursor();
    *clean = 0xB4000000u | ((uint32_t)(((complete - (uint8_t *)clean) / 4) & 0x7FFFF) << 5) | 16;
}
