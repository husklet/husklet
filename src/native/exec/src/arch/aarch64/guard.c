#include "guard.h"

#include "projection.h"
#include "stub.h"

#include <stddef.h>

#define CPU 28
#define OFFSET_FIRST ((int)offsetof(hl_native_aarch64_cpu, memory_first))
#define OFFSET_LAST ((int)offsetof(hl_native_aarch64_cpu, memory_last))
#define OFFSET_DELTA ((int)offsetof(hl_native_aarch64_cpu, memory_delta))
#define OFFSET_PERMISSIONS ((int)offsetof(hl_native_aarch64_cpu, memory_permissions))
#define OFFSET_FAULT_ADDRESS ((int)offsetof(hl_native_aarch64_cpu, fault_address))
#define OFFSET_FAULT_ACCESS ((int)offsetof(hl_native_aarch64_cpu, fault_access))
#define OFFSET_FAULT_SIZE ((int)offsetof(hl_native_aarch64_cpu, fault_size))
#define OFFSET_FLAGS ((int)offsetof(hl_native_aarch64_cpu, flags))
#define OFFSET_READ_TOKEN ((int)offsetof(hl_native_aarch64_cpu, read_token))
#define OFFSET_READ_INCARNATION ((int)offsetof(hl_native_aarch64_cpu, read_incarnation))
#define OFFSET_READ_COUNT ((int)offsetof(hl_native_aarch64_cpu, read_count))
#define OFFSET_READ_VIEWS ((int)offsetof(hl_native_aarch64_cpu, read_views))
#define OFFSET_WRITTEN ((int)offsetof(hl_native_aarch64_cpu, memory_written))
#define OFFSET_EXECUTABLE_WRITTEN ((int)offsetof(hl_native_aarch64_cpu, executable_written))
#define OFFSET_DIRTY_VIEW_FIRST ((int)offsetof(hl_native_aarch64_cpu, dirty_view_first))
#define OFFSET_DIRTY_VIEW_LAST ((int)offsetof(hl_native_aarch64_cpu, dirty_view_last))
#define OFFSET_DIRTY_FIRST ((int)offsetof(hl_native_aarch64_cpu, dirty_first))
#define OFFSET_DIRTY_LAST ((int)offsetof(hl_native_aarch64_cpu, dirty_last))
#define OFFSET_DIRTY_COUNT ((int)offsetof(hl_native_aarch64_cpu, dirty_count))
#define OFFSET_DIRTY_RECORDS ((int)offsetof(hl_native_aarch64_cpu, dirty_records))
#define OFFSET_CERTIFICATE_VALID ((int)offsetof(hl_native_aarch64_cpu, certificate_valid))
#define OFFSET_CERTIFICATE_DELTA ((int)offsetof(hl_native_aarch64_cpu, certificate_delta))

static void condition(uint32_t *instruction, const uint8_t *target, unsigned value) {
    int64_t words = (target - (const uint8_t *)instruction) / 4;
    *instruction = 0x54000000u | (((uint32_t)words & 0x7ffffu) << 5) | value;
}

static void branch(uint32_t *instruction, const uint8_t *target);

void hl_a64_guard_write_begin(hl_a64_assembler *assembler, uint64_t bytes, uint64_t pc) {
    uint32_t *empty, *contiguous, *above, *below, *overflow, *safe;
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u); /* mrs x9,nzcv */
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0xCB110210u); /* sub x16,x16,x17: recover guest EA */
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_movconst(assembler, 9, UINT64_MAX);
    hl_a64_emit32(assembler, 0xEB09023Fu); /* cmp first,#UINT64_MAX */
    empty = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_LAST);
    hl_a64_emit32(assembler, 0xEB09021Fu); /* cmp address,last */
    /* Sequential stores extend the live interval and consume no journal slot. */
    contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_emit32(assembler, 0xEB11025Fu); /* cmp end,first */
    below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    safe = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *disjoint = assembler->cursor;
    condition(above, disjoint, 8u);
    condition(below, disjoint, 3u);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_COUNT);
    hl_a64_emit32(assembler, 0xF100423Fu); /* cmp count,#16 */
    overflow = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *safe_target = assembler->cursor;
    condition(empty, safe_target, 0u);
    condition(contiguous, safe_target, 0u);
    branch(safe, safe_target);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0x8B110210u); /* restore projected EA */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u); /* msr nzcv,x17 */
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    uint32_t *after_overflow = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *overflow_target = assembler->cursor;
    condition(overflow, overflow_target, 2u);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_EPOCH, pc);
    branch(after_overflow, assembler->cursor);
}

void hl_a64_guard_written(hl_a64_assembler *assembler, uint64_t bytes) {
    uint32_t *empty, *contiguous, *above, *below, *after_merge, *after_range;
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u); /* mrs x9,nzcv */
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0xCB110210u); /* recover guest EA */
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_movconst(assembler, 9, UINT64_MAX);
    hl_a64_emit32(assembler, 0xEB09023Fu);
    empty = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_LAST);
    hl_a64_emit32(assembler, 0xEB09021Fu);
    /* Commit this only after the host store above the emitted guard succeeds. */
    contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_emit32(assembler, 0xEB11025Fu);
    below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_emit32(assembler, 0xEB11021Fu);
    hl_a64_emit32(assembler, 0x9A913211u);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, 0xEB09025Fu);
    hl_a64_emit32(assembler, 0x9A898251u);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_LAST);
    after_merge = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *disjoint = assembler->cursor;
    condition(above, disjoint, 8u);
    condition(below, disjoint, 3u);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_COUNT);
    hl_a64_addi(assembler, 9, CPU, OFFSET_DIRTY_RECORDS);
    hl_a64_addlsl4(assembler, 9, 9, 17);
    hl_a64_addlsl4(assembler, 9, 9, 17);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_VIEW_FIRST);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_DIRTY_VIEW_LAST);
    hl_a64_stp(assembler, 17, 18, 9, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_DIRTY_LAST);
    hl_a64_stp(assembler, 17, 18, 9, 16);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_COUNT);
    hl_a64_addi(assembler, 17, 17, 1);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_COUNT);
    uint8_t *set = assembler->cursor;
    condition(empty, set, 0u);
    hl_a64_str(assembler, 16, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_str(assembler, 18, CPU, OFFSET_DIRTY_LAST);
    branch(after_merge, assembler->cursor);
    after_range = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *contiguous_target = assembler->cursor;
    condition(contiguous, contiguous_target, 0u);
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_str(assembler, 18, CPU, OFFSET_DIRTY_LAST);
    uint8_t *range_done = assembler->cursor;
    branch(after_range, range_done);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FIRST);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_VIEW_FIRST);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_LAST);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_VIEW_LAST);
    hl_a64_movconst(assembler, 17, 1);
    hl_a64_str(assembler, 17, CPU, OFFSET_WRITTEN);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_PERMISSIONS);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_EXECUTABLE_WRITTEN);
    hl_a64_emit32(assembler, 0xAA120231u);
    hl_a64_str(assembler, 17, CPU, OFFSET_EXECUTABLE_WRITTEN);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
}

static void test(uint32_t *instruction, const uint8_t *target, unsigned bit) {
    int64_t words = (target - (const uint8_t *)instruction) / 4;
    *instruction = 0x36000000u | ((bit & 0x20u) << 26) | ((bit & 31u) << 19) |
                   (((uint32_t)words & 0x3fffu) << 5) | 17u;
}

static void branch(uint32_t *instruction, const uint8_t *target) {
    int64_t words = (target - (const uint8_t *)instruction) / 4;
    *instruction = 0x14000000u | ((uint32_t)words & 0x03ffffffu);
}

static void cbnz(uint32_t *instruction, const uint8_t *target, unsigned reg) {
    int64_t words = (target - (const uint8_t *)instruction) / 4;
    *instruction = UINT32_C(0xb5000000) | (((uint32_t)words & UINT32_C(0x7ffff)) << 5) | reg;
}

static void read_cache(hl_a64_assembler *assembler, uint64_t bytes, uint32_t **hits) {
    uint32_t *active[4 + 1];
    uint32_t *next[4][4];
    uint8_t *starts[4];
    unsigned active_count = 0;

    hl_a64_addi(assembler, 9, 16, (unsigned)bytes);
    hl_a64_emit32(assembler, 0xEB10013Fu); /* cmp x9,x16: end wrapped */
    active[active_count++] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_READ_TOKEN);
    hl_a64_emit32(assembler, 0xF100023Fu); /* cmp x17,#0 */
    active[active_count++] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_emit32(assembler, 0xD50339BFu); /* dmb ishld: acquire published views */
    hl_a64_ldr(assembler, 18, CPU, OFFSET_READ_INCARNATION);
    hl_a64_emit32(assembler, 0xEB12023Fu); /* cmp x17,x18 */
    active[active_count++] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_READ_COUNT);
    hl_a64_emit32(assembler, 0xF100123Fu); /* cmp x17,#4 */
    active[active_count++] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    for (unsigned index = 0; index < 4; ++index) {
        int base = OFFSET_READ_VIEWS + (int)(index * 4u * sizeof(uint64_t));
        starts[index] = assembler->cursor;
        hl_a64_emit32(assembler, 0xF100023Fu | ((index + 1u) << 10)); /* cmp count,#index+1 */
        next[index][0] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 18, CPU, base);
        hl_a64_emit32(assembler, 0xEB12021Fu); /* cmp guest EA,first */
        next[index][1] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 18, CPU, base + (int)sizeof(uint64_t));
        hl_a64_emit32(assembler, 0xEB12013Fu); /* cmp end,last */
        next[index][2] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 17, CPU, base + 3 * (int)sizeof(uint64_t));
        next[index][3] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 17, CPU, base + 2 * (int)sizeof(uint64_t));
        hl_a64_emit32(assembler, 0x8B110210u); /* add projected EA */
        hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
        hl_a64_emit32(assembler, 0xD51B4200u | 17u);
        hl_a64_ldr(assembler, 9, CPU, 9 * 8);
        hits[index] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
    }

    uint8_t *active_target = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(active[0], active_target, 3u);
    condition(active[1], active_target, 0u);
    condition(active[2], active_target, 1u);
    condition(active[3], active_target, 8u);
    for (unsigned index = 0; index < 4; ++index) {
        const uint8_t *following = index + 1u < 4 ? starts[index + 1u] : active_target;
        condition(next[index][0], active_target, 3u);
        condition(next[index][1], following, 3u);
        condition(next[index][2], following, 8u);
        test(next[index][3], following, 0u);
    }
}

/* A write miss may be an alternation between already projected stack and heap
 * views. Resolve those immutable views inside the trace, but also install the
 * selected view as the active write owner so exact dirty intervals retain the
 * correct projection identity. The slow callback remains authoritative for a
 * view absent from this bounded, generation-qualified cache. */
static void write_cache(hl_a64_assembler *assembler, uint64_t bytes, const uint8_t *resume) {
    uint32_t *inactive[4];
    uint32_t *next[4][4];
    uint8_t *starts[4];

    hl_a64_addi(assembler, 9, 16, (unsigned)bytes);
    hl_a64_emit32(assembler, 0xEB10013Fu); /* cmp end,address: wrapped */
    inactive[0] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_READ_TOKEN);
    hl_a64_emit32(assembler, 0xF100023Fu); /* cmp token,#0 */
    inactive[1] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_emit32(assembler, 0xD50339BFu); /* dmb ishld */
    hl_a64_ldr(assembler, 18, CPU, OFFSET_READ_INCARNATION);
    hl_a64_emit32(assembler, 0xEB12023Fu); /* cmp token,incarnation */
    inactive[2] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_READ_COUNT);
    hl_a64_emit32(assembler, 0xF100123Fu); /* cmp count,#4 */
    inactive[3] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    for (unsigned index = 0; index < 4; ++index) {
        int base = OFFSET_READ_VIEWS + (int)(index * 4u * sizeof(uint64_t));
        starts[index] = assembler->cursor;
        hl_a64_emit32(assembler, 0xF100023Fu | ((index + 1u) << 10));
        next[index][0] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 18, CPU, base);
        hl_a64_emit32(assembler, 0xEB12021Fu); /* cmp address,first */
        next[index][1] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 18, CPU, base + (int)sizeof(uint64_t));
        hl_a64_emit32(assembler, 0xEB12013Fu); /* cmp end,last */
        next[index][2] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        hl_a64_ldr(assembler, 17, CPU, base + 3 * (int)sizeof(uint64_t));
        next[index][3] = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);

        hl_a64_ldr(assembler, 18, CPU, base);
        hl_a64_str(assembler, 18, CPU, OFFSET_FIRST);
        hl_a64_ldr(assembler, 18, CPU, base + (int)sizeof(uint64_t));
        hl_a64_str(assembler, 18, CPU, OFFSET_LAST);
        hl_a64_ldr(assembler, 17, CPU, base + 2 * (int)sizeof(uint64_t));
        hl_a64_str(assembler, 17, CPU, OFFSET_DELTA);
        hl_a64_emit32(assembler, 0x8B110210u); /* add projected EA */
        hl_a64_ldr(assembler, 18, CPU, base + 3 * (int)sizeof(uint64_t));
        hl_a64_str(assembler, 18, CPU, OFFSET_PERMISSIONS);
        hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
        hl_a64_emit32(assembler, 0xD51B4200u | 17u);
        hl_a64_ldr(assembler, 9, CPU, 9 * 8);
        uint32_t *retry = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        branch(retry, resume);
    }

    uint8_t *miss = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(inactive[0], miss, 3u);
    condition(inactive[1], miss, 0u);
    condition(inactive[2], miss, 1u);
    condition(inactive[3], miss, 8u);
    for (unsigned index = 0; index < 4; ++index) {
        const uint8_t *following = index + 1u < 4 ? starts[index + 1u] : miss;
        condition(next[index][0], miss, 3u);
        condition(next[index][1], following, 3u);
        condition(next[index][2], following, 8u);
        test(next[index][3], following, 1u);
    }
}

static void legacy_begin(hl_a64_assembler *assembler, uint64_t bytes, uint32_t required,
                         hl_a64_guard *guard) {
    uint32_t *cache_hits[4] = {0};
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u);
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    if (required == HL_A64_PERMISSION_READ) read_cache(assembler, bytes, cache_hits);
    if (!hl_a64_assembler_ok(assembler)) return;
    hl_a64_ldr(assembler, 9, CPU, OFFSET_FIRST);
    hl_a64_emit32(assembler, 0xEB09021Fu);
    guard->below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_addi(assembler, 9, 16, (unsigned)bytes);
    hl_a64_emit32(assembler, 0xEB10013Fu);
    guard->overflow = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_LAST);
    hl_a64_emit32(assembler, 0xEB11013Fu);
    guard->above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_PERMISSIONS);
    guard->permission = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0x8B110210u);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    if (required == HL_A64_PERMISSION_READ)
        for (unsigned index = 0; index < 4; ++index)
            branch(cache_hits[index], assembler->cursor);
    guard->resume = assembler->cursor;
    guard->required = required;
    guard->bytes = bytes;
}

void hl_a64_guard_begin_mode(hl_a64_assembler *assembler, uint64_t bytes, uint32_t required,
                             hl_a64_guard_mode mode, hl_a64_guard *guard) {
    if (mode != HL_A64_GUARD_AUTHENTICATED_MEMBER) {
        legacy_begin(assembler, bytes, required, guard);
        return;
    }
    /* A zero certificate falls through to the complete legacy guard.  A valid
     * certificate may skip only repeated range/permission/delta selection;
     * trace entry has already authenticated and published the exact active
     * view fields required by write reservation and dirty publication. */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_CERTIFICATE_VALID);
    uint32_t *valid = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    legacy_begin(assembler, bytes, required, guard);
    uint32_t *done = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *valid_target = assembler->cursor;
    cbnz(valid, valid_target, 17u);
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, UINT32_C(0xd53b4209));
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_CERTIFICATE_DELTA);
    hl_a64_emit32(assembler, UINT32_C(0x8b110210));
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, UINT32_C(0xd51b4211));
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    branch(done, assembler->cursor);
}

void hl_a64_guard_begin(hl_a64_assembler *assembler, uint64_t bytes, uint32_t required,
                        hl_a64_guard *guard) {
    hl_a64_guard_begin_mode(assembler, bytes, required, HL_A64_GUARD_LEGACY, guard);
}

void hl_a64_guard_direct_begin(hl_a64_assembler *assembler, uint64_t bytes, uint32_t required,
                               hl_a64_guard *guard) {
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u);
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_FIRST);
    hl_a64_emit32(assembler, 0xEB09021Fu); /* cmp address,first */
    guard->below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_addi(assembler, 9, 16, (unsigned)bytes);
    hl_a64_emit32(assembler, 0xEB10013Fu); /* cmp end,address */
    guard->overflow = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_LAST);
    hl_a64_emit32(assembler, 0xEB11013Fu); /* cmp end,last */
    guard->above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_PERMISSIONS);
    guard->permission = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0x8B110210u);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    guard->resume = assembler->cursor;
    guard->required = required;
    guard->bytes = bytes;
}

void hl_a64_guard_finish(hl_a64_assembler *assembler, const hl_a64_guard *guard) {
    uint8_t *miss = assembler->cursor;
    condition(guard->below, miss, 3);
    condition(guard->overflow, miss, 3);
    condition(guard->above, miss, 8);
    test(guard->permission, miss, guard->required == HL_A64_PERMISSION_READ ? 0u : 1u);
    if (guard->required == HL_A64_PERMISSION_WRITE)
        write_cache(assembler, guard->bytes, guard->resume);
    if (!hl_a64_assembler_ok(assembler)) return;
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    hl_a64_str(assembler, 16, CPU, OFFSET_FAULT_ADDRESS);
    hl_a64_movconst(assembler, 17, guard->required);
    hl_a64_str(assembler, 17, CPU, OFFSET_FAULT_ACCESS);
    hl_a64_movconst(assembler, 17, guard->bytes);
    hl_a64_str(assembler, 17, CPU, OFFSET_FAULT_SIZE);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_FALLBACK, guard->pc);
}
