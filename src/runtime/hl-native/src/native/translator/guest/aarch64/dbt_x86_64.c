/* AArch64 guest on an x86-64 host.
 *
 * Generated code pins the canonical AArch64 cpu record in host r15.  The first
 * lowering deliberately reads and writes cpu->x[] instead of pretending the
 * 31-register guest has an x86 register allocation.  That makes MOVZ/MOVK and
 * SVC sufficient for an honest first block while every dispatcher, signal and
 * checkpoint boundary continues to see the existing canonical representation.
 *
 * Unsupported blocks remain interpreter-owned. Supported blocks publish
 * through the same target-owned cache, so code generation cannot introduce a
 * second host stack or a second cpu-state/checkpoint contract.
 */

#include "../../host/x86_asm.h"

enum {
    HL_A64_X86_CPU_REG = 15,
    HL_A64_X86_VALUE_REG = 0,
};

_Static_assert(offsetof(struct cpu, x) == 0, "AArch64 x86 DBT x[] offset drifted");
_Static_assert(offsetof(struct cpu, sp) == OFF_SP, "AArch64 x86 DBT sp offset drifted");
_Static_assert(offsetof(struct cpu, pc) == 256, "AArch64 x86 DBT pc offset drifted");
_Static_assert(offsetof(struct cpu, reason) == 272, "AArch64 x86 DBT reason offset drifted");
_Static_assert(offsetof(struct cpu, host_sp) == 280, "AArch64 x86 DBT host-sp offset drifted");
_Static_assert(offsetof(struct cpu, nzcv) == OFF_NZCV, "AArch64 x86 DBT nzcv offset drifted");
_Static_assert(R_SYSCALL == 1, "AArch64 x86 DBT syscall reason drifted");

#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)
extern uint64_t hl_a64_x86_dbt_enter(struct cpu *cpu, void *code) __attribute__((visibility("hidden")));
extern void hl_a64_x86_dbt_return(void) __attribute__((visibility("hidden")));

__asm__(".pushsection .text\n.p2align 4\n"
        ".hidden hl_a64_x86_dbt_enter\n"
        ".type hl_a64_x86_dbt_enter,@function\n"
        "hl_a64_x86_dbt_enter:\n"
        "push %rbx\npush %rbp\npush %r12\npush %r13\npush %r14\npush %r15\n"
        "mov %rsp,280(%rdi)\n"
        "mov %rdi,%r15\n"
        "jmp *%rsi\n"
        ".size hl_a64_x86_dbt_enter,.-hl_a64_x86_dbt_enter\n"
        ".p2align 4\n"
        ".hidden hl_a64_x86_dbt_return\n"
        ".type hl_a64_x86_dbt_return,@function\n"
        "hl_a64_x86_dbt_return:\n"
        "mov 280(%r15),%rsp\n"
        "pop %r15\npop %r14\npop %r13\npop %r12\npop %rbp\npop %rbx\n"
        "ret\n"
        ".size hl_a64_x86_dbt_return,.-hl_a64_x86_dbt_return\n"
        ".popsection\n");
#endif

/* The MOV-wide lowering's complete cpu-memory vocabulary.  MOVZ materializes
 * into rax then stores; MOVK loads, masks/inserts, and stores.  Keeping these
 * wrappers based on the shared bounds-checked assembler makes overflow an
 * abandoned block, never a truncated executable block. */
static inline void hl_a64_x86_load_x(hl_x64_asm *assembler, int guest_register) {
    hl_x64_reg_mem_disp32(assembler, 0x8B, HL_A64_X86_VALUE_REG, HL_A64_X86_CPU_REG,
                          guest_register * (int)sizeof(uint64_t));
}

static inline void hl_a64_x86_store_x(hl_x64_asm *assembler, int guest_register) {
    hl_x64_reg_mem_disp32(assembler, 0x89, HL_A64_X86_VALUE_REG, HL_A64_X86_CPU_REG,
                          guest_register * (int)sizeof(uint64_t));
}

static inline void hl_a64_x86_load_gpr_sp(hl_x64_asm *assembler, unsigned guest_register) {
    hl_x64_reg_mem_disp32(assembler, 0x8B, HL_A64_X86_VALUE_REG, HL_A64_X86_CPU_REG,
                          guest_register == 31u ? OFF_SP : (int)guest_register * (int)sizeof(uint64_t));
}

static inline void hl_a64_x86_store_gpr_sp(hl_x64_asm *assembler, unsigned guest_register) {
    hl_x64_reg_mem_disp32(assembler, 0x89, HL_A64_X86_VALUE_REG, HL_A64_X86_CPU_REG,
                          guest_register == 31u ? OFF_SP : (int)guest_register * (int)sizeof(uint64_t));
}

/* Keep the complete interpreter as the fallback translator.  Rename only its
 * three dispatcher seam symbols; all architectural helpers remain shared. */
#define translate_block hl_a64_interp_translate_block
#define run_block hl_a64_interp_run_block
#define block_return hl_a64_interp_block_return
#include "interp.c"
#undef block_return
#undef run_block
#undef translate_block

/* Match the same-ISA translator's fetch signature while retaining the shared
 * guest-fetch authority used by the interpreter composition. */
static uint32_t a64_fetch_instruction(uint64_t guest, int *ok) {
    uint32_t instruction = 0;
    int success = hl_guest_fetch_u32(guest, &instruction) == 0;
    if (ok != NULL) *ok = success;
    return success ? instruction : 0;
}

static inline void hl_a64_x86_and_reg(hl_x64_asm *assembler, int destination, int source) {
    hl_x64_u8(assembler, (uint8_t)(0x48 | ((source >= 8) ? 4 : 0) | ((destination >= 8) ? 1 : 0)));
    hl_x64_u8(assembler, 0x21);
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((source & 7) << 3) | (destination & 7)));
}

static inline void hl_a64_x86_or_reg(hl_x64_asm *assembler, int destination, int source) {
    hl_x64_u8(assembler, (uint8_t)(0x48 | ((source >= 8) ? 4 : 0) | ((destination >= 8) ? 1 : 0)));
    hl_x64_u8(assembler, 0x09);
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((source & 7) << 3) | (destination & 7)));
}

static inline void hl_a64_x86_sub_reg(hl_x64_asm *assembler, int destination, int source) {
    hl_x64_u8(assembler, (uint8_t)(0x48 | ((source >= 8) ? 4 : 0) | ((destination >= 8) ? 1 : 0)));
    hl_x64_u8(assembler, 0x29);
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((source & 7) << 3) | (destination & 7)));
}

static int hl_a64_x86_emit_pc_relative(hl_x64_asm *assembler, uint32_t instruction, uint64_t guest_pc) {
    if ((instruction & 0x1F000000u) != 0x10000000u) return 0;
    unsigned destination = instruction & 31u;
    int64_t immediate = interp_sext((((instruction >> 5) & 0x7FFFFu) << 2) |
                                        ((instruction >> 29) & 3u),
                                    21);
    uint64_t base = pcrel_base(guest_pc);
    uint64_t value = instruction & 0x80000000u
                         ? (base & ~UINT64_C(0xFFF)) + ((uint64_t)immediate << 12)
                         : base + (uint64_t)immediate;
    if (destination != 31u) {
        hl_x64_mov_imm64(assembler, HL_A64_X86_VALUE_REG, value);
        hl_a64_x86_store_x(assembler, (int)destination);
    }
    return 1;
}

static int hl_a64_x86_emit_add_sub_immediate(hl_x64_asm *assembler, uint32_t instruction) {
    if ((instruction & 0x3F000000u) != 0x11000000u) return 0; /* excludes flag-setting forms */
    unsigned sf = instruction >> 31;
    unsigned subtract = (instruction >> 30) & 1u;
    unsigned source = (instruction >> 5) & 31u;
    unsigned destination = instruction & 31u;
    uint64_t immediate = (instruction >> 10) & 0xFFFu;
    if (instruction & (1u << 22)) immediate <<= 12;
    hl_a64_x86_load_gpr_sp(assembler, source);
    hl_x64_mov_imm64(assembler, 1, immediate);
    if (subtract)
        hl_a64_x86_sub_reg(assembler, HL_A64_X86_VALUE_REG, 1);
    else
        hl_x64_add_reg(assembler, HL_A64_X86_VALUE_REG, 1);
    if (!sf) {
        hl_x64_mov_imm64(assembler, 1, UINT64_C(0xFFFFFFFF));
        hl_a64_x86_and_reg(assembler, HL_A64_X86_VALUE_REG, 1);
    }
    hl_a64_x86_store_gpr_sp(assembler, destination);
    return 1;
}

static void hl_a64_x86_load_gpr(hl_x64_asm *assembler, int host_register, unsigned guest_register,
                                int sp_allowed) {
    if (guest_register == 31u && !sp_allowed) {
        hl_x64_mov_imm64(assembler, host_register, 0);
        return;
    }
    int offset = guest_register == 31u ? OFF_SP : (int)guest_register * (int)sizeof(uint64_t);
    hl_x64_reg_mem_disp32(assembler, 0x8B, host_register, HL_A64_X86_CPU_REG, offset);
}

static void hl_a64_x86_store_gpr(hl_x64_asm *assembler, int host_register, unsigned guest_register,
                                 int sp_allowed) {
    if (guest_register == 31u && !sp_allowed) return;
    int offset = guest_register == 31u ? OFF_SP : (int)guest_register * (int)sizeof(uint64_t);
    hl_x64_reg_mem_disp32(assembler, 0x89, host_register, HL_A64_X86_CPU_REG, offset);
}

static void hl_a64_x86_emit_binary(hl_x64_asm *assembler, uint8_t opcode, unsigned sf) {
    if (sf) hl_x64_u8(assembler, 0x48);
    hl_x64_u8(assembler, opcode);
    hl_x64_u8(assembler, 0xC8); /* operation %rcx,%rax (or 32-bit equivalents) */
}

static void hl_a64_x86_emit_neg(hl_x64_asm *assembler, int host_register, unsigned sf) {
    if (sf || host_register >= 8)
        hl_x64_u8(assembler, (uint8_t)(0x40 | (sf ? 8 : 0) | (host_register >= 8 ? 1 : 0)));
    hl_x64_u8(assembler, 0xF7);
    hl_x64_u8(assembler, (uint8_t)(0xD8 | (host_register & 7))); /* NEG r/m32|64 */
}

static void hl_a64_x86_emit_imul(hl_x64_asm *assembler, int destination, int source, unsigned sf) {
    if (sf || destination >= 8 || source >= 8)
        hl_x64_u8(assembler, (uint8_t)(0x40 | (sf ? 8 : 0) |
                                        (destination >= 8 ? 4 : 0) | (source >= 8 ? 1 : 0)));
    hl_x64_u8(assembler, 0x0F);
    hl_x64_u8(assembler, 0xAF);
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((destination & 7) << 3) | (source & 7)));
}

static void hl_a64_x86_emit_sign_extend32(hl_x64_asm *assembler, int host_register) {
    hl_x64_u8(assembler, (uint8_t)(0x48 | (host_register >= 8 ? 5 : 0)));
    hl_x64_u8(assembler, 0x63); /* MOVSXD r64,r/m32 */
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((host_register & 7) << 3) | (host_register & 7)));
}

static void hl_a64_x86_emit_zero_extend32(hl_x64_asm *assembler, int host_register) {
    if (host_register >= 8) hl_x64_u8(assembler, 0x45);
    hl_x64_u8(assembler, 0x89); /* MOV r32,r32 clears the destination's high half. */
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((host_register & 7) << 3) | (host_register & 7)));
}

static void hl_a64_x86_emit_high_multiply(hl_x64_asm *assembler, int source, int is_signed) {
    hl_x64_u8(assembler, (uint8_t)(0x48 | (source >= 8 ? 1 : 0)));
    hl_x64_u8(assembler, 0xF7);
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((is_signed ? 5 : 4) << 3) | (source & 7)));
}

static void hl_a64_x86_emit_shift(hl_x64_asm *assembler, int host_register, unsigned operation,
                                  unsigned amount, unsigned sf) {
    if (!amount) return;
    if (sf) hl_x64_u8(assembler, (uint8_t)(0x48 | (host_register >= 8 ? 1 : 0)));
    else if (host_register >= 8)
        hl_x64_u8(assembler, 0x41);
    hl_x64_u8(assembler, 0xC1);
    hl_x64_u8(assembler, (uint8_t)(0xC0 | ((operation & 7u) << 3) | (host_register & 7)));
    hl_x64_u8(assembler, (uint8_t)amount);
}

static void hl_a64_x86_emit_setcc(hl_x64_asm *assembler, int host_register, unsigned condition) {
    if (host_register >= 4) hl_x64_u8(assembler, (uint8_t)(0x40 | (host_register >= 8 ? 1 : 0)));
    hl_x64_u8(assembler, 0x0F);
    hl_x64_u8(assembler, (uint8_t)(0x90 | (condition & 15u)));
    hl_x64_u8(assembler, (uint8_t)(0xC0 | (host_register & 7)));
}

static void hl_a64_x86_prepare_nzcv(hl_x64_asm *assembler) {
    for (int reg = 2; reg <= 8; reg += reg == 2 ? 4 : 1) {
        if (reg >= 8) hl_x64_u8(assembler, 0x45);
        hl_x64_u8(assembler, 0x31);
        hl_x64_u8(assembler, (uint8_t)(0xC0 | ((reg & 7) << 3) | (reg & 7)));
    }
}

static void hl_a64_x86_emit_nzcv(hl_x64_asm *assembler, int subtract, int logical) {
    /* Scratch registers were zeroed before the arithmetic operation.
     * Consecutive SETcc instructions preserve its flags, so all four host
     * conditions are captured before shifts normalize architectural NZCV. */
    hl_a64_x86_emit_setcc(assembler, 2, 0);                 /* V: OF */
    if (!logical)
        hl_a64_x86_emit_setcc(assembler, 6, subtract ? 3 : 2); /* C: no-borrow/carry */
    hl_a64_x86_emit_setcc(assembler, 7, 4);                 /* Z: ZF */
    hl_a64_x86_emit_setcc(assembler, 8, 8);                 /* N: SF */
    hl_x64_shl_imm8(assembler, 2, 28);
    hl_x64_shl_imm8(assembler, 6, 29);
    hl_x64_shl_imm8(assembler, 7, 30);
    hl_x64_shl_imm8(assembler, 8, 31);
    hl_a64_x86_or_reg(assembler, 2, 6);
    hl_a64_x86_or_reg(assembler, 2, 7);
    hl_a64_x86_or_reg(assembler, 2, 8);
    hl_x64_reg_mem_disp32(assembler, 0x89, 2, HL_A64_X86_CPU_REG, OFF_NZCV);
}

static int hl_a64_x86_emit_stage_two_alu(hl_x64_asm *assembler, uint32_t instruction) {
    unsigned sf = instruction >> 31;
    unsigned amount = (instruction >> 10) & 0x3Fu;

    /* ADDS/SUBS immediate. Rn names SP, Rd names ZR; the oracle leaf owns
     * precisely that distinction and canonical N:Z:C:V placement. */
    if ((instruction & 0x3F000000u) == 0x31000000u) {
        unsigned subtract = (instruction >> 30) & 1u;
        uint64_t immediate = (instruction >> 10) & 0xFFFu;
        if (instruction & (1u << 22)) immediate <<= 12;
        hl_a64_x86_load_gpr(assembler, 0, (instruction >> 5) & 31u, 1);
        hl_x64_mov_imm64(assembler, 1, immediate);
        hl_a64_x86_prepare_nzcv(assembler);
        hl_a64_x86_emit_binary(assembler, subtract ? 0x29 : 0x01, sf);
        hl_a64_x86_emit_nzcv(assembler, (int)subtract, 0);
        hl_a64_x86_store_gpr(assembler, 0, instruction & 31u, 0);
        return 1;
    }

    /* Logical immediate, including ANDS. DecodeBitMasks rejects reserved
     * element shapes before any generated prefix can be published. */
    if ((instruction & 0x1F800000u) == 0x12000000u) {
        uint64_t ignored;
        if (!interp_bit_masks(sf, (instruction >> 22) & 1u, (instruction >> 10) & 0x3Fu,
                              (instruction >> 16) & 0x3Fu, 1, &ignored, NULL))
            return 0;
        unsigned opc = (instruction >> 29) & 3u;
        hl_a64_x86_load_gpr(assembler, 0, (instruction >> 5) & 31u, 0);
        hl_x64_mov_imm64(assembler, 1, ignored);
        if (opc == 3u) hl_a64_x86_prepare_nzcv(assembler);
        hl_a64_x86_emit_binary(assembler, opc == 0u || opc == 3u ? 0x21 : opc == 1u ? 0x09 : 0x31, sf);
        if (opc == 3u) hl_a64_x86_emit_nzcv(assembler, 0, 1);
        hl_a64_x86_store_gpr(assembler, 0, instruction & 31u, opc != 3u);
        return 1;
    }

    /* ADD/SUB shifted register only: bit21 distinguishes the deliberately
     * deferred extended-register family, and ROR is architecturally invalid. */
    if ((instruction & 0x1F200000u) == 0x0B000000u) {
        unsigned shift_type = (instruction >> 22) & 3u;
        if (shift_type == 3u || (!sf && (amount & 0x20u))) return 0;
        unsigned subtract = (instruction >> 30) & 1u;
        unsigned setflags = (instruction >> 29) & 1u;
        hl_a64_x86_load_gpr(assembler, 0, (instruction >> 5) & 31u, 0);
        hl_a64_x86_load_gpr(assembler, 1, (instruction >> 16) & 31u, 0);
        hl_a64_x86_emit_shift(assembler, 1, shift_type == 0u ? 4u : shift_type == 1u ? 5u : 7u, amount, sf);
        if (setflags) hl_a64_x86_prepare_nzcv(assembler);
        hl_a64_x86_emit_binary(assembler, subtract ? 0x29 : 0x01, sf);
        if (setflags) hl_a64_x86_emit_nzcv(assembler, (int)subtract, 0);
        hl_a64_x86_store_gpr(assembler, 0, instruction & 31u, 0);
        return 1;
    }

    /* Non-inverting AND/ORR/EOR/ANDS shifted register. BIC/ORN/EON/BICS
     * remain interpreter-owned until their own measured stage. */
    if ((instruction & 0x1F200000u) == 0x0A000000u) {
        if (!sf && (amount & 0x20u)) return 0;
        unsigned opc = (instruction >> 29) & 3u;
        unsigned shift_type = (instruction >> 22) & 3u;
        hl_a64_x86_load_gpr(assembler, 0, (instruction >> 5) & 31u, 0);
        hl_a64_x86_load_gpr(assembler, 1, (instruction >> 16) & 31u, 0);
        hl_a64_x86_emit_shift(assembler, 1,
                              shift_type == 0u ? 4u : shift_type == 1u ? 5u : shift_type == 2u ? 7u : 1u,
                              amount, sf);
        if (opc == 3u) hl_a64_x86_prepare_nzcv(assembler);
        hl_a64_x86_emit_binary(assembler, opc == 0u || opc == 3u ? 0x21 : opc == 1u ? 0x09 : 0x31, sf);
        if (opc == 3u) hl_a64_x86_emit_nzcv(assembler, 0, 1);
        hl_a64_x86_store_gpr(assembler, 0, instruction & 31u, 0);
        return 1;
    }
    return 0;
}

/* Data-processing (3 source). The interpreter decoder below is the complete
 * architectural owner for admission: mirror every allocated op31/o0/sf shape
 * here so no unallocated encoding can acquire a generated prefix. NZCV is
 * untouched by the whole family. x31 is ZR for all four operand fields. */
static int hl_a64_x86_emit_three_source(hl_x64_asm *assembler, uint32_t instruction) {
    if ((instruction & 0x1F000000u) != 0x1B000000u) return 0;
    unsigned sf = instruction >> 31;
    unsigned op31 = (instruction >> 21) & 7u;
    unsigned o0 = (instruction >> 15) & 1u;
    unsigned destination = instruction & 31u;
    unsigned source1 = (instruction >> 5) & 31u;
    unsigned source2 = (instruction >> 16) & 31u;
    unsigned addend = (instruction >> 10) & 31u;

    if ((op31 == 1u || op31 == 5u) && !sf) return 0;
    if ((op31 == 2u || op31 == 6u) && (!sf || o0)) return 0;
    if (op31 != 0u && op31 != 1u && op31 != 2u && op31 != 5u && op31 != 6u) return 0;

    hl_a64_x86_load_gpr(assembler, 0, source1, 0);
    hl_a64_x86_load_gpr(assembler, 1, source2, 0);
    if (op31 == 2u || op31 == 6u) {
        hl_a64_x86_emit_high_multiply(assembler, 1, op31 == 2u);
        hl_x64_mov_reg(assembler, 0, 2); /* high half: %rdx -> %rax */
    } else {
        if (op31 == 1u) {
            hl_a64_x86_emit_sign_extend32(assembler, 0);
            hl_a64_x86_emit_sign_extend32(assembler, 1);
        } else if (op31 == 5u) {
            hl_a64_x86_emit_zero_extend32(assembler, 0);
            hl_a64_x86_emit_zero_extend32(assembler, 1);
        }
        hl_a64_x86_emit_imul(assembler, 0, 1, sf);
        hl_a64_x86_load_gpr(assembler, 1, addend, 0);
        if (o0) hl_a64_x86_emit_neg(assembler, 0, sf);
        hl_a64_x86_emit_binary(assembler, 0x01, sf); /* product + Ra, or -product + Ra */
    }
    hl_a64_x86_store_gpr(assembler, 0, destination, 0);
    return 1;
}

static int hl_a64_x86_emit_mov_wide(hl_x64_asm *assembler, uint32_t instruction) {
    if ((instruction & 0x1F800000u) != 0x12800000u) return 0;
    unsigned sf = instruction >> 31;
    unsigned opc = (instruction >> 29) & 3u;
    unsigned half = (instruction >> 21) & 3u;
    unsigned destination = instruction & 31u;
    if ((opc != 2u && opc != 3u) || (!sf && half >= 2u)) return 0;

    /* x31 is ZR for move-wide instructions: the write retires but vanishes. */
    if (destination == 31u) return 1;
    uint64_t insert = (uint64_t)((instruction >> 5) & 0xFFFFu) << (half * 16u);
    if (opc == 2u) {
        hl_x64_mov_imm64(assembler, HL_A64_X86_VALUE_REG, sf ? insert : (uint32_t)insert);
    } else {
        hl_a64_x86_load_x(assembler, (int)destination);
        hl_x64_mov_imm64(assembler, 1, ~(UINT64_C(0xFFFF) << (half * 16u)));
        hl_a64_x86_and_reg(assembler, HL_A64_X86_VALUE_REG, 1);
        hl_x64_mov_imm64(assembler, 1, insert);
        hl_a64_x86_or_reg(assembler, HL_A64_X86_VALUE_REG, 1);
        if (!sf) {
            hl_x64_mov_imm64(assembler, 1, UINT64_C(0xFFFFFFFF));
            hl_a64_x86_and_reg(assembler, HL_A64_X86_VALUE_REG, 1);
        }
    }
    hl_a64_x86_store_x(assembler, (int)destination);
    return 1;
}

static int hl_a64_x86_is_svc(uint32_t instruction) {
    return (instruction & 0xFFE0001Fu) == 0xD4000001u;
}

static void hl_a64_x86_emit_cpu_u64(hl_x64_asm *assembler, int offset, uint64_t value);

static int hl_a64_x86_is_ldrb_post(uint32_t instruction) {
    return (instruction & (1u << 26)) == 0 &&
           (instruction & 0x3B200000u) == 0x38000000u &&
           (instruction >> 30) == 0u && ((instruction >> 22) & 3u) == 1u &&
           ((instruction >> 10) & 3u) == 1u;
}

/* The overwhelmingly common scalar memory instruction in the measured C
 * workloads. Keep the canonical accessor -- it owns non-PIE projection,
 * SIGBUS ledger accounting and the per-access signal marker -- while avoiding
 * the generic single-transfer decoder on every execution. Architectural
 * writes remain ordered load destination, writeback, then next PC. */
static void hl_a64_x86_exec_ldrb_post(struct cpu *cpu, uint32_t instruction) {
    int rt = (int)(instruction & 31u), rn = (int)((instruction >> 5) & 31u);
    uint64_t base = interp_gpr_sp(cpu, rn);
    int64_t offset = interp_sext((instruction >> 12) & 0x1FFu, 9);
    uint64_t value = interp_load_bits(base, 1);
    interp_set_gpr32(cpu, rt, (uint32_t)value);
    interp_set_gpr_sp(cpu, rn, base + (uint64_t)offset);
    cpu->pc += 4;
}

/* Scalar integer single-register transfers share one architectural decoder,
 * but three address forms. Keep admission identical to that decoder so an
 * emitted helper can only retire INTERP_NEXT: SIMD, pointer authentication and
 * unallocated sign-extension forms remain interpreter-owned. */
static int hl_a64_x86_is_scalar_single_memory(uint32_t instruction) {
    if (instruction & (1u << 26)) return 0;
    int scaled = (instruction & 0x3B000000u) == 0x39000000u;
    int register_offset = (instruction & 0x3B200C00u) == 0x38200800u;
    int unscaled = (instruction & 0x3B200000u) == 0x38000000u;
    if (!scaled && !register_offset && !unscaled) return 0;
    if ((instruction & 0x3B200C00u) == 0x38200400u ||
        (instruction & 0x3B200C00u) == 0x38200C00u)
        return 0;
    if (register_offset && (((instruction >> 13) & 3u) < 2u)) return 0;
    unsigned size = instruction >> 30;
    unsigned opc = (instruction >> 22) & 3u;
    return !(opc == 3u && size >= 2u);
}

static void hl_a64_x86_emit_scalar_single_memory(hl_x64_asm *assembler, uint32_t instruction,
                                                  uint64_t guest_pc) {
    /* The canonical interpreter accessor owns address projection, unaligned
     * little-endian transfers, BUS accounting and SP/ZR/writeback semantics.
     * One run_block landing pad below owns synchronous-fault recovery for all
     * helper calls in this generated block. */
    hl_a64_x86_emit_cpu_u64(assembler, OFF_PC, guest_pc);
    hl_x64_mov_reg(assembler, 7, HL_A64_X86_CPU_REG); /* cpu -> %rdi */
    hl_x64_mov_imm64(assembler, 6, instruction);      /* insn -> %rsi */
    hl_x64_u8(assembler, 0x48); hl_x64_u8(assembler, 0x83);
    hl_x64_u8(assembler, 0xEC); hl_x64_u8(assembler, 8); /* align stack */
    uintptr_t helper = hl_a64_x86_is_ldrb_post(instruction)
                           ? (uintptr_t)hl_a64_x86_exec_ldrb_post
                           : (uintptr_t)interp_exec_load_store_single;
    hl_x64_mov_imm64(assembler, 11, helper);
    hl_x64_u8(assembler, 0x41); hl_x64_u8(assembler, 0xFF); hl_x64_u8(assembler, 0xD3); /* call *%r11 */
    hl_x64_u8(assembler, 0x48); hl_x64_u8(assembler, 0x83);
    hl_x64_u8(assembler, 0xC4); hl_x64_u8(assembler, 8);
}

static void hl_a64_x86_emit_cpu_u64(hl_x64_asm *assembler, int offset, uint64_t value) {
    hl_x64_mov_imm64(assembler, HL_A64_X86_VALUE_REG, value);
    hl_x64_reg_mem_disp32(assembler, 0x89, HL_A64_X86_VALUE_REG, HL_A64_X86_CPU_REG, offset);
}

#define HL_A64_X86_BLOCK_MAGIC UINT64_C(0x484C413658444254) /* "HLA6XDBT" */
struct hl_a64_x86_block_header {
    uint64_t magic;
    uint64_t retired_steps;
    uint64_t exit_kind;
    uint64_t reserved;
    uint64_t branch_target;
    uint64_t branch_fallthrough;
    uint64_t loop_steps;
    uint64_t reserved2;
};
_Static_assert(sizeof(struct hl_a64_x86_block_header) % 16u == 0, "AArch64 x86 DBT entry lost alignment");

enum { HL_A64_X86_MAX_BLOCK_BYTES = 4096 };
_Static_assert(HL_A64_X86_MAX_BLOCK_BYTES <= CACHE_EMIT_HEADROOM,
               "AArch64 x86 DBT block exceeds dispatcher cache admission");

static void hl_a64_x86_emit_return(hl_x64_asm *assembler) {
    /* r14 counts completed in-body backedges and is the return value used to
     * reconcile dynamically retired guest instructions. */
    hl_x64_u8(assembler, 0x4C); hl_x64_u8(assembler, 0x89); hl_x64_u8(assembler, 0xF0); /* mov %r14,%rax */
    hl_x64_mov_imm64(assembler, 1, (uintptr_t)hl_a64_x86_dbt_return);
    hl_x64_u8(assembler, 0xFF);
    hl_x64_u8(assembler, 0xE1); /* jmp *%rcx */
}

static uint8_t *hl_a64_x86_emit_jcc32(hl_x64_asm *assembler, unsigned condition) {
    hl_x64_u8(assembler, 0x0F);
    hl_x64_u8(assembler, (uint8_t)(0x80 | (condition & 15u)));
    uint8_t *displacement = assembler->cursor;
    hl_x64_u32(assembler, 0);
    return displacement;
}

static void hl_a64_x86_patch_rel32(uint8_t *displacement, const uint8_t *target) {
    intptr_t relative = target - (displacement + 4);
    if (relative < INT32_MIN || relative > INT32_MAX) abort();
    int32_t encoded = (int32_t)relative;
    memcpy(displacement, &encoded, sizeof encoded);
}

enum { HL_A64_X86_BACKEDGE_BUDGET = 8 };
_Static_assert(HL_A64_X86_BACKEDGE_BUDGET == 8,
               "AArch64 x86 DBT poll budget must match dispatcher redispatch budget");
enum { HL_A64_X86_MAX_BLOCK_INSNS = 64 };
_Static_assert(HL_A64_X86_MAX_BLOCK_INSNS +
                       (HL_A64_X86_BACKEDGE_BUDGET - 1) * HL_A64_X86_MAX_BLOCK_INSNS <=
                   UINT16_MAX,
               "AArch64 x86 DBT dynamic retired count must not overflow");

static int hl_a64_x86_emit_conditional_terminal(hl_x64_asm *assembler, uint32_t instruction,
                                                 uint64_t cursor, uint64_t *target_out,
                                                 uint8_t *direct_target) {
    int64_t displacement;
    unsigned branch_condition;
    if ((instruction & 0xFF000010u) == 0x54000000u ||
        (instruction & 0xFF000010u) == 0x54000010u) {
        displacement = interp_sext((instruction >> 5) & 0x7FFFFu, 19) * 4;
        struct cpu condition_cpu = {0};
        uint16_t truth = 0;
        for (unsigned nzcv = 0; nzcv < 16u; ++nzcv) {
            condition_cpu.nzcv = (uint64_t)nzcv << 28;
            if (interp_cond_holds(&condition_cpu, instruction & 15u)) truth |= (uint16_t)(1u << nzcv);
        }
        hl_x64_reg_mem_disp32(assembler, 0x8B, 0, HL_A64_X86_CPU_REG, OFF_NZCV);
        hl_x64_shift_imm8(assembler, 0, 5, 28);
        hl_x64_mov_imm64(assembler, 1, truth);
        hl_x64_u8(assembler, 0x48); /* bt %rax,%rcx */
        hl_x64_u8(assembler, 0x0F);
        hl_x64_u8(assembler, 0xA3);
        hl_x64_u8(assembler, 0xC1);
        branch_condition = 2; /* JC */
    } else if ((instruction & 0x7E000000u) == 0x34000000u) {
        unsigned sf = instruction >> 31;
        displacement = interp_sext((instruction >> 5) & 0x7FFFFu, 19) * 4;
        hl_a64_x86_load_gpr(assembler, 0, instruction & 31u, 0);
        if (sf) hl_x64_u8(assembler, 0x48);
        hl_x64_u8(assembler, 0x85); /* test %rax,%rax / %eax,%eax */
        hl_x64_u8(assembler, 0xC0);
        branch_condition = ((instruction >> 24) & 1u) ? 5u : 4u; /* JNZ / JZ */
    } else if ((instruction & 0x7E000000u) == 0x36000000u) {
        unsigned bit = ((instruction >> 31) & 1u) << 5 | ((instruction >> 19) & 31u);
        displacement = interp_sext((instruction >> 5) & 0x3FFFu, 14) * 4;
        hl_a64_x86_load_gpr(assembler, 0, instruction & 31u, 0);
        hl_x64_u8(assembler, 0x48); /* bt $bit,%rax */
        hl_x64_u8(assembler, 0x0F);
        hl_x64_u8(assembler, 0xBA);
        hl_x64_u8(assembler, 0xE0);
        hl_x64_u8(assembler, (uint8_t)bit);
        branch_condition = ((instruction >> 24) & 1u) ? 2u : 3u; /* JC / JNC */
    } else {
        return 0;
    }

    uint8_t *taken_patch = hl_a64_x86_emit_jcc32(assembler, branch_condition);
    hl_a64_x86_emit_cpu_u64(assembler, OFF_PC, cursor + 4);
    hl_a64_x86_emit_cpu_u64(assembler, OFF_RSN, R_BRANCH);
    hl_a64_x86_emit_return(assembler);
    if (!assembler->overflow) hl_a64_x86_patch_rel32(taken_patch, assembler->cursor);
    *target_out = cursor + (uint64_t)displacement;
    if (direct_target != NULL) {
        /* The condition's host flags are consumed by taken_patch before this
         * counter compare changes them; guest NZCV remains canonical in cpu.
         * Escape on the same eight-edge budget
         * used by dispatcher redispatch so IRQ, signal, checkpoint and SMC
         * invalidation observe a safepoint within a bounded interval. */
        hl_x64_u8(assembler, 0x49); hl_x64_u8(assembler, 0x83); hl_x64_u8(assembler, 0xFE);
        hl_x64_u8(assembler, HL_A64_X86_BACKEDGE_BUDGET - 1); /* cmp $7,%r14 */
        uint8_t *budget_patch = hl_a64_x86_emit_jcc32(assembler, 3); /* JAE external exit */
        hl_x64_u8(assembler, 0x49); hl_x64_u8(assembler, 0xFF); hl_x64_u8(assembler, 0xC6); /* inc %r14 */
        hl_x64_u8(assembler, 0xE9);
        uint8_t *loop_patch = assembler->cursor;
        hl_x64_u32(assembler, 0);
        if (!assembler->overflow) hl_a64_x86_patch_rel32(loop_patch, direct_target);
        if (!assembler->overflow) hl_a64_x86_patch_rel32(budget_patch, assembler->cursor);
    }
    hl_a64_x86_emit_cpu_u64(assembler, OFF_PC, *target_out);
    hl_a64_x86_emit_cpu_u64(assembler, OFF_RSN, R_BRANCH);
    hl_a64_x86_emit_return(assembler);
    return 1;
}

static int hl_a64_x86_conditional_is_next(uint32_t instruction) {
    if ((instruction & 0xFF000010u) == 0x54000000u ||
        (instruction & 0xFF000010u) == 0x54000010u ||
        (instruction & 0x7E000000u) == 0x34000000u)
        return ((instruction >> 5) & 0x7FFFFu) == 1u;
    if ((instruction & 0x7E000000u) == 0x36000000u)
        return ((instruction >> 5) & 0x3FFFu) == 1u;
    return 0;
}

static void *translate_block(uint64_t guest_pc) {
    /* map_put/txpg_mark describe one non-wrapping source interval. AArch64
     * address arithmetic wraps, but a block spanning UINT64_MAX cannot be
     * represented by that cache contract, so leave it to the interpreter. */
    if (guest_pc >= UINT64_MAX - UINT64_C(0xFFF))
        return hl_a64_interp_translate_block(guest_pc);
    uint64_t source_page = guest_pc & ~UINT64_C(0xFFF);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));
    uint8_t *const begin = g_cp;
    while ((uintptr_t)g_cp & 15u)
        *g_cp++ = 0x90;
    struct hl_a64_x86_block_header *const header = (struct hl_a64_x86_block_header *)g_cp;
    g_cp += sizeof *header;
    uint8_t *const entry = g_cp;
    uint8_t *const cache_end = g_cache + CACHE_SZ;
    if ((size_t)(cache_end - entry) < HL_A64_X86_MAX_BLOCK_BYTES) {
        g_cp = begin;
        return hl_a64_interp_translate_block(guest_pc);
    }
    hl_x64_asm assembler = {
        .cursor = entry,
        .end = entry + HL_A64_X86_MAX_BLOCK_BYTES,
        .overflow = 0,
    };
    uint64_t cursor = guest_pc;
    uint64_t source_end = guest_pc;
    int has_memory = 0;
    uint8_t *host_for_instruction[HL_A64_X86_MAX_BLOCK_INSNS] = {0};
    uint64_t guest_for_instruction[HL_A64_X86_MAX_BLOCK_INSNS] = {0};
    /* r14 is callee-saved by the entry trampoline and unused by the ALU
     * lowering. It counts only completed direct backedges. */
    hl_x64_u8(&assembler, 0x45); hl_x64_u8(&assembler, 0x31); hl_x64_u8(&assembler, 0xF6); /* xor %r14d,%r14d */

    /* A generated prefix is published only when a supported terminal is present.
     * Otherwise rewind the arena and let the interpreter translate the
     * ORIGINAL PC; no emitted prefix has executed or retired. */
    /* Translation follows an unseen one-way forward B, then stops at the first
     * remaining terminal. A published body therefore has at most one direct
     * backedge even when its target lies inside a nested guest loop. */
    for (unsigned count = 0; count < HL_A64_X86_MAX_BLOCK_INSNS; ++count, cursor += 4) {
        host_for_instruction[count] = assembler.cursor;
        guest_for_instruction[count] = cursor;
        int fetch_ok = 0;
        uint32_t instruction = a64_fetch_instruction(cursor, &fetch_ok);
        if (!fetch_ok) break;
        if (cursor + 4 > source_end) source_end = cursor + 4;
        if (hl_a64_x86_emit_mov_wide(&assembler, instruction)) continue;
        if (hl_a64_x86_emit_pc_relative(&assembler, instruction, cursor)) continue;
        if (hl_a64_x86_emit_add_sub_immediate(&assembler, instruction)) continue;
        if (hl_a64_x86_emit_stage_two_alu(&assembler, instruction)) continue;
        if (hl_a64_x86_emit_three_source(&assembler, instruction)) continue;
        if (hl_a64_x86_is_scalar_single_memory(instruction)) {
            hl_a64_x86_emit_scalar_single_memory(&assembler, instruction, cursor);
            has_memory = 1;
            continue;
        }
        if ((instruction & 0xFC000000u) == 0x14000000u) {
            int64_t displacement = interp_sext(instruction & 0x3FFFFFFu, 26) * 4;
            uint64_t target = cursor + (uint64_t)displacement;
            if (displacement == 4) continue; /* B to the next instruction. */
            int seen = 0;
            for (unsigned index = 0; index <= count; ++index)
                if (guest_for_instruction[index] == target) seen = 1;
            /* Follow one-way forward control inside the same source page. The
             * skipped interval stays in the published source hull, making SMC
             * invalidation conservative. Backward/repeated targets remain a
             * terminal and use the bounded backedge path below. */
            if (target > cursor && (target & ~UINT64_C(0xFFF)) == source_page && !seen &&
                count + 1 < HL_A64_X86_MAX_BLOCK_INSNS) {
                cursor = target - 4;
                continue;
            }
        }
        if (hl_a64_x86_conditional_is_next(instruction))
            continue; /* Both conditional outcomes are the next instruction. */
        uint64_t exit_kind;
        uint64_t conditional_target = 0;
        uint8_t *direct_target = NULL;
        uint64_t direct_target_loop_steps = 0;
        uint64_t decoded_target = 0;
        if ((instruction & 0xFF000010u) == 0x54000000u ||
            (instruction & 0xFF000010u) == 0x54000010u ||
            (instruction & 0x7E000000u) == 0x34000000u)
            decoded_target = cursor + (uint64_t)(interp_sext((instruction >> 5) & 0x7FFFFu, 19) * 4);
        else if ((instruction & 0x7E000000u) == 0x36000000u)
            decoded_target = cursor + (uint64_t)(interp_sext((instruction >> 5) & 0x3FFFu, 14) * 4);
        if (decoded_target < cursor)
            for (unsigned index = 0; index < count; ++index)
                if (guest_for_instruction[index] == decoded_target) {
                    direct_target = host_for_instruction[index];
                    direct_target_loop_steps = (uint64_t)count - index + 1;
                    break;
                }
        int conditional = hl_a64_x86_emit_conditional_terminal(&assembler, instruction, cursor,
                                                                &conditional_target, direct_target);
        if (conditional) {
            exit_kind = UINT64_MAX;
        } else if (hl_a64_x86_is_svc(instruction)) {
            hl_a64_x86_emit_cpu_u64(&assembler, OFF_PC, cursor);
            hl_a64_x86_emit_cpu_u64(&assembler, OFF_RSN, R_SYSCALL);
            exit_kind = HL_BACKEND_SHAPE_T_SYSCALL;
        } else if ((instruction & 0xFC000000u) == 0x14000000u) {
            int64_t displacement = interp_sext(instruction & 0x3FFFFFFu, 26) * 4;
            hl_a64_x86_emit_cpu_u64(&assembler, OFF_PC, cursor + (uint64_t)displacement);
            hl_a64_x86_emit_cpu_u64(&assembler, OFF_RSN, R_BRANCH);
            exit_kind = HL_BACKEND_SHAPE_T_DIRECT_JUMP;
        } else {
            /* Mechanism observation only: this is the first instruction that
             * makes the candidate prefix interpreter-owned. Count once per
             * rejected translation attempt, never once per execution. */
            hl_backend_tree_a64_unsupported(instruction);
            break;
        }
        if (!conditional) hl_a64_x86_emit_return(&assembler);
        if (assembler.overflow) break;
        header->magic = HL_A64_X86_BLOCK_MAGIC;
        header->retired_steps = (uint64_t)count + 1;
        header->exit_kind = exit_kind;
        header->reserved = (uint64_t)has_memory;
        header->branch_target = conditional_target;
        header->branch_fallthrough = conditional ? cursor + 4 : 0;
        header->loop_steps = direct_target_loop_steps;
        header->reserved2 = 0;
        g_cp = assembler.cursor;
        if (map_put(guest_pc, guest_pc, source_end, entry, entry) != MAP_PUT_OK) {
            static const char message[] = "AArch64 x86 DBT translation map is full";
            g_cp = begin;
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            return NULL;
        }
        txpg_mark(guest_pc, source_end);
        if (g_txln_active)
            for (uint64_t line = guest_pc >> 6; line <= (source_end - 1) >> 6; ++line)
                txln_put(line);
        return entry;
    }

    g_cp = begin;
    return hl_a64_interp_translate_block(guest_pc);
}

static inline void hl_a64_x86_record_translated_exit(unsigned kind) {
#if defined(HL_NATIVE_TEST_HOOKS)
    hl_backend_tree_translated_exit(kind, 0, 0);
#else
    hl_backend_tree_translated_exit_count(kind);
#endif
}

static void run_block(struct cpu *cpu, void *code) {
    /* Both representations are wholly inside a map entry: interpreter blocks
     * begin with an eight-byte descriptor magic. Generated blocks begin with
     * either movabs (48 b8: MOV-wide/ADR or a zero-register ALU source) or a
     * cpu-record load (49 8b: ADD/SUB/ALU), while a terminal-only block also
     * begins with movabs.
     * None can equal descriptor magic (little-endian 54 42), and reading the
     * first word is within the published source object in either case. */
    const struct interp_block *descriptor = (const struct interp_block *)code;
    if (descriptor->magic == INTERP_BLOCK_MAGIC) {
        hl_a64_interp_run_block(cpu, code);
    } else {
        const struct hl_a64_x86_block_header *header =
            (const struct hl_a64_x86_block_header *)code - 1;
        if (header->magic != HL_A64_X86_BLOCK_MAGIC) {
            static const char message[] = "AArch64 x86 DBT entered a foreign block";
            (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
            cpu->reason = R_BRANCH;
            return;
        }
        if (header->reserved > 1) {
            static const char message[] = "AArch64 x86 DBT entered a corrupt memory marker";
            (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
            cpu->reason = R_BRANCH;
            return;
        }
        /* Register state is canonical at every helper boundary. One marker for
         * the whole translated entry lets the existing memory access seam
         * abandon a faulting transfer without partially committing it. */
        int has_memory = header->reserved != 0;
        if (has_memory && sigsetjmp(g_interp_marker_jmp, 0) != 0) {
            g_interp_access_active = 0;
            g_interp_marker_armed = 0;
            g_interp_marker_cpu = NULL;
            hl_backend_tree_reason(cpu->reason);
            return;
        }
        if (has_memory) {
            g_interp_marker_cpu = cpu;
            g_interp_marker_armed = 1;
        }
        uint64_t repetitions = hl_a64_x86_dbt_enter(cpu, code);
        if (has_memory) {
            g_interp_access_active = 0;
            g_interp_marker_armed = 0;
            g_interp_marker_cpu = NULL;
        }
        if (repetitions >= HL_A64_X86_BACKEDGE_BUDGET ||
            (header->loop_steps == 0 && repetitions != 0)) {
            static const char message[] = "AArch64 x86 DBT returned invalid backedge accounting";
            (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
            cpu->reason = R_BRANCH;
            return;
        }
        hl_backend_tree_run_begin(1, header->retired_steps + repetitions * header->loop_steps);
        unsigned exit_kind = (unsigned)header->exit_kind;
        if (header->exit_kind == UINT64_MAX) {
            if (cpu->pc == header->branch_target)
                exit_kind = HL_BACKEND_SHAPE_T_COND_TAKEN;
            else if (cpu->pc == header->branch_fallthrough)
                exit_kind = HL_BACKEND_SHAPE_T_COND_NOT_TAKEN;
            else {
                static const char message[] = "AArch64 x86 DBT conditional returned an impossible PC";
                (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
                cpu->reason = R_BRANCH;
                return;
            }
        }
        hl_a64_x86_record_translated_exit(exit_kind);
        hl_backend_tree_reason(cpu->reason);
    }
}

static void block_return(void) {
    hl_a64_interp_block_return();
}
