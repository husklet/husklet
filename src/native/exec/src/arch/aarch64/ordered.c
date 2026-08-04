#include "ordered.h"

#include "projection.h"
#include "stub.h"

#include <string.h>

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int decode(uint32_t word, unsigned *bytes, unsigned *load,
                  unsigned *base, unsigned *target) {
    if ((word & UINT32_C(0x3fbf7c00)) != UINT32_C(0x089f7c00)) return 0;
    *bytes = 1u << ((word >> 30) & 3u);
    *load = (word >> 22) & 1u;
    *base = (word >> 5) & 31u;
    *target = word & 31u;
    return 1;
}

static void address(hl_a64_assembler *assembler, unsigned base) {
    if (base == 31)
        hl_a64_mov_from_sp(assembler, 16);
    else if (stolen(base))
        hl_a64_ldr(assembler, 16, CPU, (int)base * 8);
    else
        hl_a64_movr(assembler, 16, (int)base);
}

int hl_a64_ordered_body(hl_a64_assembler *assembler, uint32_t word,
                        uint64_t pc, hl_a64_guard *guard, hl_a64_memory_sites *sites) {
    unsigned bytes, load, base, target;
    unsigned native_target;
    int target_stolen;
    if (assembler == NULL || guard == NULL ||
        !decode(word, &bytes, &load, &base, &target)) return 0;
    memset(guard, 0, sizeof(*guard));
    if (sites != NULL) memset(sites, 0, sizeof(*sites));
    guard->pc = pc;
    target_stolen = target != 31 && stolen(target);
    native_target = target_stolen ? 17u : target;
    address(assembler, base);
    hl_a64_guard_begin_mode(assembler, bytes,
                            load ? HL_A64_PERMISSION_READ : HL_A64_PERMISSION_WRITE,
                            HL_A64_GUARD_LEGACY, guard);
    if (!load) hl_a64_guard_write_begin(assembler, bytes, pc);
    if (!load && target_stolen)
        hl_a64_ldr(assembler, 17, CPU, (int)target * 8);
    if (sites != NULL) {
        sites->count = 1;
        sites->entries[0] = (hl_a64_memory_site){
            .code_offset = hl_a64_assembler_size(assembler),
            .access = load ? HL_NATIVE_ACCESS_READ : HL_NATIVE_ACCESS_WRITE,
            .width = bytes,
        };
    }
    hl_a64_emit32(assembler, (word & ~UINT32_C(0x3ff)) |
                               (16u << 5) | native_target);
    if (!load) hl_a64_guard_written(assembler, bytes);
    if (load && target_stolen)
        hl_a64_str(assembler, 17, CPU, (int)target * 8);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_ordered_emit(hl_a64_assembler *assembler, uint32_t word,
                        uint64_t pc) {
    unsigned bytes, load, base, target;
    hl_a64_guard guard;
    if (assembler == NULL ||
        hl_a64_assembler_remaining(assembler) < HL_A64_ORDERED_MAX_BYTES ||
        !decode(word, &bytes, &load, &base, &target)) return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_ordered_body(assembler, word, pc, &guard, NULL)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    hl_a64_guard_finish(assembler, &guard);
    return hl_a64_assembler_ok(assembler);
}
