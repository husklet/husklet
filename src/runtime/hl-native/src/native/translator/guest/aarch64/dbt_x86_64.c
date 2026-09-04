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
_Static_assert(offsetof(struct cpu, pc) == 256, "AArch64 x86 DBT pc offset drifted");
_Static_assert(offsetof(struct cpu, reason) == 272, "AArch64 x86 DBT reason offset drifted");
_Static_assert(offsetof(struct cpu, host_sp) == 280, "AArch64 x86 DBT host-sp offset drifted");
_Static_assert(R_SYSCALL == 1, "AArch64 x86 DBT syscall reason drifted");

#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)
extern void hl_a64_x86_dbt_enter(struct cpu *cpu, void *code) __attribute__((visibility("hidden")));
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

static void hl_a64_x86_emit_cpu_u64(hl_x64_asm *assembler, int offset, uint64_t value) {
    hl_x64_mov_imm64(assembler, HL_A64_X86_VALUE_REG, value);
    hl_x64_reg_mem_disp32(assembler, 0x89, HL_A64_X86_VALUE_REG, HL_A64_X86_CPU_REG, offset);
}

#define HL_A64_X86_BLOCK_MAGIC UINT64_C(0x484C413658444254) /* "HLA6XDBT" */
struct hl_a64_x86_block_header {
    uint64_t magic;
    uint64_t retired_steps;
};

enum { HL_A64_X86_MAX_BLOCK_BYTES = 4096 };
_Static_assert(HL_A64_X86_MAX_BLOCK_BYTES <= CACHE_EMIT_HEADROOM,
               "AArch64 x86 DBT block exceeds dispatcher cache admission");

static void hl_a64_x86_emit_return(hl_x64_asm *assembler) {
    hl_x64_mov_imm64(assembler, 1, (uintptr_t)hl_a64_x86_dbt_return);
    hl_x64_u8(assembler, 0xFF);
    hl_x64_u8(assembler, 0xE1); /* jmp *%rcx */
}

static void *translate_block(uint64_t guest_pc) {
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

    /* A generated prefix is published only when its terminal SVC is present.
     * Otherwise rewind the arena and let the interpreter translate the
     * ORIGINAL PC; no emitted prefix has executed or retired. */
    for (unsigned count = 0; count < 64u; ++count, cursor += 4) {
        int fetch_ok = 0;
        uint32_t instruction = a64_fetch_instruction(cursor, &fetch_ok);
        if (!fetch_ok) break;
        if (hl_a64_x86_emit_mov_wide(&assembler, instruction)) continue;
        if (!hl_a64_x86_is_svc(instruction)) break;
        hl_a64_x86_emit_cpu_u64(&assembler, OFF_PC, cursor);
        hl_a64_x86_emit_cpu_u64(&assembler, OFF_RSN, R_SYSCALL);
        hl_a64_x86_emit_return(&assembler);
        if (assembler.overflow) break;
        header->magic = HL_A64_X86_BLOCK_MAGIC;
        header->retired_steps = (uint64_t)count + 1;
        g_cp = assembler.cursor;
        if (map_put(guest_pc, guest_pc, cursor + 4, entry, entry) != MAP_PUT_OK) {
            static const char message[] = "AArch64 x86 DBT translation map is full";
            g_cp = begin;
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            return NULL;
        }
        txpg_mark(guest_pc, cursor + 4);
        if (g_txln_active)
            for (uint64_t line = guest_pc >> 6; line <= cursor >> 6; ++line)
                txln_put(line);
        return entry;
    }

    g_cp = begin;
    return hl_a64_interp_translate_block(guest_pc);
}

static void run_block(struct cpu *cpu, void *code) {
    /* Both representations are wholly inside a map entry: interpreter blocks
     * begin with an eight-byte descriptor magic, while generated blocks begin
     * with movabs (48 b8) because MOV-wide and the terminal stores both
     * materialize through rax. Consequently generated bytes cannot equal the
     * descriptor magic (little-endian 54 42), and reading the first word is
     * within the published source object in either case. */
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
        /* MOV-wide touches registers and the two terminal stores touch only
         * the always-live cpu record. There is no guest-memory instruction in
         * this slice, hence no synchronous-fault provenance interval to add. */
        hl_backend_tree_run_begin(1, header->retired_steps);
        hl_a64_x86_dbt_enter(cpu, code);
        hl_backend_tree_reason(cpu->reason);
    }
}

static void block_return(void) {
    hl_a64_interp_block_return();
}
