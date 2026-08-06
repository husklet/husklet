#include "pair.h"

#include "guard.h"
#include "memory.h"
#include "projection.h"
#include "stub.h"

#include <stddef.h>

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

static int valid(uint32_t word, uint64_t pc, hl_a64_memory *memory) {
    if (!hl_a64_memory_decode(word, pc, memory) || memory->kind != HL_A64_MEMORY_PAIR ||
        (memory->vector && ((word >> 30) & 3u) == 3u)) return 0;
    return 1;
}

int hl_a64_pair_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc, hl_a64_guard *guard,
                     hl_a64_memory_sites *sites) {
    hl_a64_memory memory;
    int first_stolen;
    int second_stolen;
    unsigned first_native;
    unsigned second_native;
    int64_t writeback;
    uint32_t native;
    if (assembler == NULL || guard == NULL || !valid(word, pc, &memory)) return 0;
    *guard = (hl_a64_guard){.pc = pc};
    if (sites != NULL) *sites = (hl_a64_memory_sites){0};
    first_stolen = !memory.vector && stolen(memory.target);
    second_stolen = !memory.vector && stolen(memory.target2);
    first_native = first_stolen ? 17u : memory.target;
    second_native = second_stolen ? (first_stolen ? 18u : 17u) : memory.target2;
    writeback = sign_extend((word >> 15) & 0x7fu, 7) * (int64_t)(memory.bytes / 2);

    if (memory.base == 31)
        hl_a64_mov_from_sp(assembler, 16);
    else if (stolen(memory.base))
        hl_a64_ldr(assembler, 16, CPU, memory.base * 8);
    else
        hl_a64_movr(assembler, 16, memory.base);
    if (!memory.postindex) add_immediate(assembler, 16, 16, memory.offset);

    hl_a64_guard_begin(assembler, memory.bytes,
                       memory.read ? HL_A64_PERMISSION_READ : HL_A64_PERMISSION_WRITE, guard);

    native = word & ~((UINT32_C(0x7f) << 15) | (3u << 23) | (31u << 10) | (31u << 5) | 31u);
    native |= (2u << 23) | (16u << 5) |
              first_native | (second_native << 10);
    if (!memory.read) hl_a64_guard_write_begin(assembler, memory.bytes, pc, guard);
    if (!memory.read) {
        if (first_stolen) hl_a64_ldr(assembler, 17, CPU, memory.target * 8);
        if (second_stolen) hl_a64_ldr(assembler, (int)second_native, CPU, memory.target2 * 8);
    }
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
    if (memory.read) {
        if (first_stolen) hl_a64_str(assembler, 17, CPU, memory.target * 8);
        if (second_stolen) hl_a64_str(assembler, (int)second_native, CPU, memory.target2 * 8);
    }
    if (memory.writeback) {
        /* Reconstruct writeback from the successful access address.  This
         * preserves the original base when a load also targets that register,
         * and the guard's miss path bypasses publication entirely.  Store
         * journaling already leaves x16 as the guest access address; loads
         * still carry the projected host address here. */
        if (memory.read) {
            hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
            hl_a64_emit32(assembler, UINT32_C(0xcb110210)); /* sub x16,x16,x17 */
        }
        if (memory.postindex) add_immediate(assembler, 16, 16, writeback);
        if (memory.base == 31)
            hl_a64_mov_sp_from(assembler, 16);
        else if (stolen(memory.base))
            hl_a64_str(assembler, 16, CPU, memory.base * 8);
        else
            hl_a64_movr(assembler, memory.base, 16);
    }
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_pair_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    hl_a64_memory memory;
    hl_a64_guard guard;
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_PAIR_MAX_BYTES ||
        !valid(word, pc, &memory)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_pair_body(assembler, word, pc, &guard, NULL)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    hl_a64_guard_finish(assembler, &guard);
    return hl_a64_assembler_ok(assembler);
}
