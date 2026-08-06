#include "zero.h"

#include "projection.h"

#define CPU 28

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

int hl_a64_zero_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc,
                     hl_a64_guard *guard, hl_a64_memory_sites *sites) {
    unsigned source = word & 31u;
    if (assembler == NULL || guard == NULL || (word & 0xffffffe0u) != 0xd50b7420u) return 0;
    *guard = (hl_a64_guard){.pc = pc};
    if (sites != NULL) *sites = (hl_a64_memory_sites){0};
    if (source == 31)
        hl_a64_movz(assembler, 16, 0, 0);
    else if (stolen(source))
        hl_a64_ldr(assembler, 16, CPU, (int)source * 8);
    else
        hl_a64_movr(assembler, 16, (int)source);
    hl_a64_emit32(assembler, 0x927AE610u); /* and x16,x16,#-64 */
    hl_a64_guard_begin_mode(assembler, 64, HL_A64_PERMISSION_WRITE,
                            HL_A64_GUARD_LEGACY, guard);
    hl_a64_guard_write_begin(assembler, 64, pc, guard);
    for (int offset = 0; offset < 64; offset += 16) {
        if (sites != NULL) {
            hl_a64_memory_site *site = &sites->entries[sites->count++];
            *site = (hl_a64_memory_site){
                .code_offset = hl_a64_assembler_size(assembler),
                .displacement = offset,
                .access = HL_NATIVE_ACCESS_WRITE,
                .width = 16,
            };
        }
        hl_a64_stp(assembler, 31, 31, 16, offset);
    }
    hl_a64_guard_written(assembler, 64);
    return hl_a64_assembler_ok(assembler);
}
