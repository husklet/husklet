#include "single.h"

#include "guard.h"
#include "memory.h"
#include "projection.h"
#include "stub.h"

#include <stddef.h>
#include <string.h>

#define CPU 28
#define OFFSET_DELTA ((int)offsetof(hl_native_aarch64_cpu, memory_delta))

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int64_t sign_extend(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1);
    return (int64_t)((value ^ sign) - sign);
}

static void add_immediate(hl_a64_assembler *assembler, int destination, int source, int64_t value) {
    uint64_t magnitude = value < 0 ? (uint64_t)(-(value + 1)) + 1 : (uint64_t)value;
    int input = source;
    while (magnitude != 0) {
        unsigned chunk = magnitude > 4095 ? 4095 : (unsigned)magnitude;
        if (value < 0)
            hl_a64_subi(assembler, destination, input, chunk);
        else
            hl_a64_addi(assembler, destination, input, chunk);
        input = destination;
        magnitude -= chunk;
    }
    if (destination != source && value == 0) hl_a64_movr(assembler, destination, source);
}

static uint32_t literal_native(uint32_t word, unsigned target) {
    uint32_t base;
    if ((word & 0xff000000u) == 0x98000000u)
        base = 0xB9800000u;
    else if ((word & 0x3f000000u) == 0x1c000000u) {
        unsigned size = (word >> 30) & 3u;
        base = size == 0 ? 0xBD400000u : size == 1 ? 0xFD400000u : 0x3DC00000u;
    } else {
        base = (word & (1u << 30)) ? 0xF9400000u : 0xB9400000u;
    }
    return base | (16u << 5) | target;
}

static void effective_address(hl_a64_assembler *assembler, uint32_t word, const hl_a64_memory *memory) {
    if (memory->kind == HL_A64_MEMORY_LITERAL) {
        hl_a64_movconst(assembler, 16, memory->pc + memory->offset);
        return;
    }
    if (memory->base == 31)
        hl_a64_mov_from_sp(assembler, 16);
    else if (stolen(memory->base))
        hl_a64_ldr(assembler, 16, CPU, memory->base * 8);
    else
        hl_a64_movr(assembler, 16, memory->base);
    if (memory->kind == HL_A64_MEMORY_REGISTER) {
        unsigned option = (word >> 13) & 7u;
        unsigned shift = ((word >> 12) & 1u) ? (unsigned)__builtin_ctzll(memory->bytes) : 0;
        if (stolen(memory->index)) {
            hl_a64_ldr(assembler, 17, CPU, memory->index * 8);
            hl_a64_emit32(assembler, 0x8B200000u | (17u << 16) | (option << 13) |
                                     (shift << 10) | (16u << 5) | 16u);
        } else {
            hl_a64_emit32(assembler, 0x8B200000u | ((uint32_t)memory->index << 16) | (option << 13) |
                                     (shift << 10) | (16u << 5) | 16u);
        }
    } else if (!memory->postindex) {
        add_immediate(assembler, 16, 16, memory->offset);
    }
}

static int valid(uint32_t word, uint64_t pc, hl_a64_memory *memory) {
    if (!hl_a64_memory_decode(word, pc, memory)) return 0;
    if (!memory->vector && ((word & UINT32_C(0x3b200c00)) == UINT32_C(0x38200400) ||
                            (word & UINT32_C(0x3b200c00)) == UINT32_C(0x38200c00)))
        return 0; /* LDRAA/LDRAB require pointer-authentication semantics. */
    if (memory->kind == HL_A64_MEMORY_REGISTER && (((word >> 13) & 3u) < 2u))
        return 0; /* Only UXTW/LSL/SXTW/SXTX are allocated for this family. */
    if (memory->kind == HL_A64_MEMORY_PREFETCH) return 1;
    if ((memory->kind != HL_A64_MEMORY_LITERAL && memory->kind != HL_A64_MEMORY_UNSIGNED &&
         memory->kind != HL_A64_MEMORY_UNSCALED && memory->kind != HL_A64_MEMORY_REGISTER) ||
        memory->bytes == 0 || memory->bytes > 16)
        return 0;
    return !(memory->writeback && memory->base != 31 && memory->read && memory->base == memory->target);
}

int hl_a64_single_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc, hl_a64_guard *guard,
                       hl_a64_memory_sites *sites) {
    hl_a64_memory memory;
    uint32_t native;
    unsigned target;
    int target_stolen;
    int64_t writeback = 0;
    if (assembler == NULL || guard == NULL || !valid(word, pc, &memory)) return 0;
    memset(guard, 0, sizeof(*guard));
    if (sites != NULL) memset(sites, 0, sizeof(*sites));
    guard->pc = pc;
    /* PRFM is an architecturally optional, non-faulting hint.  Re-emitting it
     * would address host memory rather than guest memory, so retain its only
     * required semantics by consuming it as a NOP. */
    if (memory.kind == HL_A64_MEMORY_PREFETCH) {
        hl_a64_emit32(assembler, UINT32_C(0xd503201f));
        return hl_a64_assembler_ok(assembler);
    }
    target_stolen = !memory.vector && memory.target != 31 && stolen(memory.target);
    if (memory.writeback)
        writeback = sign_extend((word >> 12) & 0x1ffu, 9);

    effective_address(assembler, word, &memory);
    hl_a64_guard_begin_mode(assembler, memory.bytes,
                            memory.read ? HL_A64_PERMISSION_READ : HL_A64_PERMISSION_WRITE,
                            HL_A64_GUARD_LEGACY, guard);
    target = target_stolen ? 17u : memory.target;
    if (memory.kind == HL_A64_MEMORY_LITERAL)
        native = literal_native(word, target);
    else
        native = (word & 0xFFC00000u) | 0x39000000u | (16u << 5) | target;
    if (!memory.read) hl_a64_guard_write_begin(assembler, memory.bytes, pc, guard);
    if (!memory.read && target_stolen) hl_a64_ldr(assembler, 17, CPU, memory.target * 8);
    if (sites != NULL) {
        sites->count = 1;
        sites->entries[0] = (hl_a64_memory_site){
            .code_offset = hl_a64_assembler_size(assembler),
            .access = memory.read ? HL_NATIVE_ACCESS_READ : HL_NATIVE_ACCESS_WRITE,
            .width = (uint32_t)memory.bytes,
        };
    }
    hl_a64_emit32(assembler, native);
    if (!memory.read) hl_a64_guard_written(assembler, memory.bytes);
    if (memory.read && target_stolen) hl_a64_str(assembler, 17, CPU, memory.target * 8);
    if (memory.writeback) {
        if (memory.base == 31) {
            add_immediate(assembler, 31, 31, writeback);
        } else if (stolen(memory.base)) {
            hl_a64_ldr(assembler, 16, CPU, memory.base * 8);
            add_immediate(assembler, 16, 16, writeback);
            hl_a64_str(assembler, 16, CPU, memory.base * 8);
        } else {
            add_immediate(assembler, memory.base, memory.base, writeback);
        }
    }
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_single_direct_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc,
                              const hl_native_direct_authority *authority,
                              hl_a64_guard *guard,
                              hl_a64_memory_sites *sites) {
    hl_a64_memory memory;
    if (assembler == NULL || authority == NULL || guard == NULL || sites == NULL ||
        !valid(word, pc, &memory) || memory.vector || memory.writeback ||
        memory.bytes == 0 || memory.bytes > 8 ||
        (memory.kind != HL_A64_MEMORY_LITERAL && memory.kind != HL_A64_MEMORY_UNSIGNED &&
         memory.kind != HL_A64_MEMORY_UNSCALED && memory.kind != HL_A64_MEMORY_REGISTER) ||
        (authority->permissions & (memory.read ? HL_NATIVE_ACCESS_READ : HL_NATIVE_ACCESS_WRITE)) == 0)
        return 0;
    memset(guard, 0, sizeof(*guard));
    guard->pc = pc;
    if (memory.kind == HL_A64_MEMORY_LITERAL) {
        uint64_t address;
        if (memory.offset >= 0) {
            if (pc > UINT64_MAX - (uint64_t)memory.offset) return 0;
            address = pc + (uint64_t)memory.offset;
        } else {
            uint64_t magnitude = (uint64_t)(-(memory.offset + 1)) + 1;
            if (pc < magnitude) return 0;
            address = pc - magnitude;
        }
        if (address < authority->guest_first || address > UINT64_MAX - memory.bytes ||
            address + memory.bytes > authority->guest_last)
            return 0;
    }
    effective_address(assembler, word, &memory);
    if (memory.kind == HL_A64_MEMORY_LITERAL) {
        hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
        hl_a64_emit32(assembler, UINT32_C(0x8B110210)); /* add x16,x16,x17 */
    } else {
        hl_a64_guard_direct_begin(assembler, memory.bytes,
                                  memory.read ? HL_A64_PERMISSION_READ : HL_A64_PERMISSION_WRITE,
                                  guard);
    }
    unsigned target = !memory.vector && memory.target != 31 && stolen(memory.target) ? 17u : memory.target;
    if (!memory.read) hl_a64_guard_write_begin(assembler, memory.bytes, pc, guard);
    if (!memory.read && target == 17u) hl_a64_ldr(assembler, 17, CPU, memory.target * 8);
    sites->count = 1;
    sites->entries[0] = (hl_a64_memory_site){
        .code_offset = hl_a64_assembler_size(assembler),
        .access = memory.read ? HL_NATIVE_ACCESS_READ : HL_NATIVE_ACCESS_WRITE,
        .width = (uint32_t)memory.bytes,
    };
    uint32_t native = memory.kind == HL_A64_MEMORY_LITERAL
        ? literal_native(word, target)
        : (word & 0xFFC00000u) | 0x39000000u | (16u << 5) | target;
    hl_a64_emit32(assembler, native);
    if (!memory.read) hl_a64_guard_written(assembler, memory.bytes);
    if (memory.read && target == 17u) hl_a64_str(assembler, 17, CPU, memory.target * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_single_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    hl_a64_memory memory;
    hl_a64_guard guard;
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_SINGLE_MAX_BYTES ||
        !valid(word, pc, &memory)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_single_body(assembler, word, pc, &guard, NULL)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    hl_a64_guard_finish(assembler, &guard);
    return hl_a64_assembler_ok(assembler);
}
