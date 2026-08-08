#include "indirect.h"

#include "stub.h"

#include <stddef.h>

#define CPU 28
#define OFFSET_SITE ((int)offsetof(hl_native_aarch64_cpu, indirect_site))
#define OFFSET_IBTC_BASE ((int)offsetof(hl_native_aarch64_cpu, ibtc_base))
#define OFFSET_BUDGET ((int)offsetof(hl_native_aarch64_cpu, budget))

/* trace_build calls hl_a64_indirect_body directly, bypassing the capacity
 * check in hl_a64_indirect_emit, so every placeholder below may sit at the
 * one-past-end cursor of an exhausted buffer. */
static void patch_cbnz(hl_a64_assembler *assembler, uint32_t *branch, const uint8_t *target) {
    uint32_t distance;
    if (!hl_a64_assembler_wrote(assembler, branch)) return;
    distance = (uint32_t)((target - (const uint8_t *)branch) / 4);
    *branch |= (distance & UINT32_C(0x7ffff)) << 5;
}

static void patch_adr(hl_a64_assembler *assembler, uint32_t *instruction, const uint8_t *target) {
    int64_t distance;
    if (!hl_a64_assembler_wrote(assembler, instruction)) return;
    distance = target - (const uint8_t *)instruction;
    *instruction |= ((uint32_t)distance & 3u) << 29;
    *instruction |= (((uint32_t)(distance >> 2)) & UINT32_C(0x7ffff)) << 5;
}

static void patch_literal(hl_a64_assembler *assembler, uint32_t *instruction, const uint8_t *target) {
    int64_t distance;
    if (!hl_a64_assembler_wrote(assembler, instruction)) return;
    distance = target - (const uint8_t *)instruction;
    *instruction |= (((uint32_t)(distance / 4)) & UINT32_C(0x7ffff)) << 5;
}

static int stolen(unsigned reg) {
    return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static void guest_register(hl_a64_assembler *assembler, int output, unsigned reg) {
    if (stolen(reg))
        hl_a64_ldr(assembler, output, CPU, (int)reg * 8);
    else
        hl_a64_movr(assembler, output, (int)reg);
}

static int valid(uint32_t word) {
    uint32_t form = word & 0xFFFFFC1Fu;
    unsigned source = (word >> 5) & 31u;
    int authenticated_return = (word & UINT32_C(0xfffffbff)) == UINT32_C(0xd65f0bff);
    return authenticated_return ||
           (source != 31 && (form == 0xD61F0000u || form == 0xD63F0000u || form == 0xD65F0000u));
}

int hl_a64_indirect_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc, void *ibtc) {
    uint32_t form = word & 0xFFFFFC1Fu;
    unsigned source = (word & UINT32_C(0xfffffbff)) == UINT32_C(0xd65f0bff)
        ? 30u
        : (word >> 5) & 31u;
    int link;
    if (assembler == NULL || !valid(word)) return 0;
    link = form == 0xD63F0000u;

    guest_register(assembler, 17, source);
    if (link) {
        hl_a64_movconst(assembler, 16, pc + 4);
        hl_a64_str(assembler, 16, CPU, 30 * 8);
    }
    if (ibtc != NULL) {
        /* The monomorphic hit uses only stolen x16/x17, so guest state stays
         * entirely live and the common path performs no guest-state memory IO. */
        uint32_t *site_target = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, UINT32_C(0x58000010)); /* ldr x16,site.target */
        hl_a64_emit32(assembler, UINT32_C(0xca110210)); /* eor x16,x16,x17 */
        uint32_t *site_miss = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, UINT32_C(0xb5000010)); /* cbnz x16,shared */
        uint32_t *site_hit = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, UINT32_C(0xd503201f)); /* patched b body; nop falls to shared */

        /* x30 carries the budget, so the shared probe publishes it and borrows
         * x30 as a third scratch rather than spilling and reloading guest x9. */
        uint8_t *shared = assembler->cursor;
        hl_a64_str(assembler, 30, CPU, OFFSET_BUDGET);
        hl_a64_emit32(assembler, UINT32_C(0xd342463e)); /* ubfx x30,x17,#2,#16 */
        hl_a64_ldr(assembler, 16, CPU, OFFSET_IBTC_BASE);
        hl_a64_addlsl4(assembler, 30, 16, 30);
        hl_a64_ldp(assembler, 30, 16, 30, 0);
        hl_a64_emit32(assembler, UINT32_C(0xca1103de)); /* eor x30,x30,x17 */
        uint32_t *shared_miss = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, UINT32_C(0xb500001e)); /* cbnz x30,miss */
        uint32_t *shared_empty = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, UINT32_C(0xb4000010)); /* cbz x16,miss */
        hl_a64_ldr(assembler, 30, CPU, OFFSET_BUDGET);
        hl_a64_br(assembler, 16);

        uint8_t *miss = assembler->cursor;
        hl_a64_ldr(assembler, 30, CPU, OFFSET_BUDGET);
        uint32_t *miss_site = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, UINT32_C(0x10000010)); /* adr x16,site */
        hl_a64_str(assembler, 16, CPU, OFFSET_SITE);
        hl_a64_stub_exit_register(assembler, HL_NATIVE_EXIT_BRANCH, 17);
        while (((size_t)(assembler->cursor - assembler->writable) & 15u) != 0)
            hl_a64_emit32(assembler, UINT32_C(0xd503201f));
        uint8_t *site = assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_emit32(assembler, 0);
        int64_t hit_delta = (uint8_t *)site_hit - site;
        hl_a64_emit32(assembler, (uint32_t)(uint64_t)hit_delta);
        hl_a64_emit32(assembler, (uint32_t)((uint64_t)hit_delta >> 32));
        patch_adr(assembler, miss_site, site);
        patch_literal(assembler, site_target, site);
        patch_cbnz(assembler, site_miss, shared);
        patch_cbnz(assembler, shared_miss, miss);
        patch_cbnz(assembler, shared_empty, miss);
        return hl_a64_assembler_ok(assembler);
    }
    hl_a64_stub_exit_register(assembler, HL_NATIVE_EXIT_BRANCH, 17);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_indirect_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_INDIRECT_MAX_BYTES || !valid(word)) return 0;
    hl_a64_stub_prologue(assembler);
    return hl_a64_indirect_body(assembler, word, pc, NULL);
}
