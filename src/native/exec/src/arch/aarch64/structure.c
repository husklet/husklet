#include "structure.h"

#include "guard.h"
#include "memory.h"
#include "projection.h"
#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static void guest_register(hl_a64_assembler *assembler, unsigned output, unsigned reg) {
    if (reg == 31)
        hl_a64_mov_from_sp(assembler, (int)output);
    else if (stolen(reg))
        hl_a64_ldr(assembler, (int)output, CPU, (int)reg * 8);
    else
        hl_a64_movr(assembler, (int)output, (int)reg);
}

static void writeback(hl_a64_assembler *assembler, const hl_a64_memory *memory) {
    unsigned increment = memory->index;
    guest_register(assembler, 16, memory->base);
    if (increment == 31) {
        hl_a64_addi(assembler, 16, 16, (unsigned)memory->bytes);
    } else {
        guest_register(assembler, 17, increment);
        hl_a64_emit32(assembler, 0x8B000000u | (17u << 16) | (16u << 5) | 16u);
    }
    if (memory->base == 31)
        hl_a64_mov_sp_from(assembler, 16);
    else if (stolen(memory->base))
        hl_a64_str(assembler, 16, CPU, (int)memory->base * 8);
    else
        hl_a64_movr(assembler, (int)memory->base, 16);
}

static int transfer_bytes(uint32_t word, uint64_t *bytes) {
    unsigned q = (word >> 30) & 1u;
    unsigned registers;
    if (((word >> 24) & 1u) == 0) {
        switch ((word >> 12) & 15u) {
            case 0:
            case 2: registers = 4; break;
            case 4:
            case 6: registers = 3; break;
            case 8:
            case 10: registers = 2; break;
            case 7: registers = 1; break;
            default: return 0;
        }
        if (((word >> 10) & 3u) == 3u && q == 0 && registers > 1) return 0;
        *bytes = (uint64_t)registers * (q ? 16u : 8u);
        return 1;
    }

    unsigned load = (word >> 22) & 1u;
    unsigned replicate_group = (word >> 21) & 1u;
    unsigned opcode = (word >> 13) & 7u;
    unsigned selector = (word >> 12) & 1u;
    unsigned size = (word >> 10) & 3u;
    unsigned element;
    registers = (replicate_group ? 2u : 1u) + ((opcode & 1u) ? 2u : 0u);
    switch (opcode >> 1) {
        case 0: element = 0; break;
        case 1:
            if ((size & 1u) != 0) return 0;
            element = 1;
            break;
        case 2:
            if (size == 0)
                element = 2;
            else if (size == 1 && selector == 0)
                element = 3;
            else
                return 0;
            break;
        default:
            if (load == 0 || selector != 0) return 0;
            element = size;
            break;
    }
    *bytes = (uint64_t)registers << element;
    return *bytes != 0 && *bytes <= 64;
}

static int valid(uint32_t word, uint64_t pc, hl_a64_memory *memory) {
    uint64_t bytes;
    if (!hl_a64_memory_decode(word, pc, memory) || memory->kind != HL_A64_MEMORY_STRUCTURE ||
        !transfer_bytes(word, &bytes))
        return 0;
    memory->bytes = bytes;
    return 1;
}

int hl_a64_structure_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc, hl_a64_guard *guard,
                          hl_a64_memory_sites *sites) {
    hl_a64_memory memory;
    uint32_t native;
    if (assembler == NULL || guard == NULL || !valid(word, pc, &memory)) return 0;
    *guard = (hl_a64_guard){.pc = pc};
    if (sites != NULL) *sites = (hl_a64_memory_sites){0};

    guest_register(assembler, 16, memory.base);
    hl_a64_guard_begin(assembler, memory.bytes,
                       memory.read ? HL_A64_PERMISSION_READ : HL_A64_PERMISSION_WRITE, guard);
    native = (word & ~(1u << 23) & ~(0x1fu << 16) & ~(0x1fu << 5)) | (16u << 5);
    if (!memory.read) hl_a64_guard_write_begin(assembler, memory.bytes, pc, guard);
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
    if (memory.writeback) writeback(assembler, &memory);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_structure_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    hl_a64_memory memory;
    hl_a64_guard guard;
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_STRUCTURE_MAX_BYTES ||
        !valid(word, pc, &memory)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_structure_body(assembler, word, pc, &guard, NULL)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    hl_a64_guard_finish(assembler, &guard);
    return hl_a64_assembler_ok(assembler);
}
