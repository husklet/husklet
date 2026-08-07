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
#define OFFSET_FAULT_COMPLETED ((int)offsetof(hl_native_aarch64_cpu, fault_completed))
#define OFFSET_FLAGS ((int)offsetof(hl_native_aarch64_cpu, flags))
#define OFFSET_READ_TOKEN ((int)offsetof(hl_native_aarch64_cpu, read_token))
#define OFFSET_READ_INCARNATION ((int)offsetof(hl_native_aarch64_cpu, read_incarnation))
#define OFFSET_READ_COUNT ((int)offsetof(hl_native_aarch64_cpu, read_count))
#define OFFSET_READ_VALID_COUNT ((int)offsetof(hl_native_aarch64_cpu, read_valid_count))
#define OFFSET_READ_VIEWS ((int)offsetof(hl_native_aarch64_cpu, read_views))
#define OFFSET_READ_VIEW_PUBLICATION ((int)offsetof(hl_native_aarch64_cpu, read_view_publication))
#define OFFSET_WRITE_POLICY ((int)offsetof(hl_native_aarch64_cpu, memory_write_policy))
#define OFFSET_WRITE_INDEX ((int)offsetof(hl_native_aarch64_cpu, memory_write_index))
#define OFFSET_WRITTEN ((int)offsetof(hl_native_aarch64_cpu, memory_written))
#define OFFSET_EXECUTABLE_WRITTEN ((int)offsetof(hl_native_aarch64_cpu, executable_written))
#define OFFSET_DIRTY_VIEW_FIRST ((int)offsetof(hl_native_aarch64_cpu, dirty_view_first))
#define OFFSET_DIRTY_VIEW_LAST ((int)offsetof(hl_native_aarch64_cpu, dirty_view_last))
#define OFFSET_DIRTY_FIRST ((int)offsetof(hl_native_aarch64_cpu, dirty_first))
#define OFFSET_DIRTY_LAST ((int)offsetof(hl_native_aarch64_cpu, dirty_last))
#define OFFSET_DIRTY_COUNT ((int)offsetof(hl_native_aarch64_cpu, dirty_count))
#define OFFSET_DIRTY_RECORDS ((int)offsetof(hl_native_aarch64_cpu, dirty_records))

/* Diagnostic translations keep their counters in the execution-local CPU
 * record. x17 is stolen state and every call site has already preserved NZCV. */
static void diagnostic_increment(hl_a64_assembler *assembler, int offset) {
    if (!assembler->diagnostics) return;
    hl_a64_ldr(assembler, 17, CPU, offset);
    hl_a64_addi(assembler, 17, 17, 1);
    hl_a64_str(assembler, 17, CPU, offset);
}

static int patch_offset(hl_a64_assembler *assembler, uint32_t *instruction,
                        const uint8_t *target, unsigned bits, int64_t *words) {
    uintptr_t writable;
    uintptr_t cursor;
    uintptr_t site;
    uintptr_t destination;
    int64_t limit;
    if (!hl_a64_assembler_ok(assembler) || instruction == NULL || target == NULL || words == NULL)
        goto invalid;
    writable = (uintptr_t)assembler->writable;
    cursor = (uintptr_t)assembler->cursor;
    site = (uintptr_t)instruction;
    destination = (uintptr_t)target;
    if ((site & 3u) != 0 || (destination & 3u) != 0 || site < writable ||
        site > cursor || cursor - site < sizeof(*instruction) ||
        destination < writable || destination > cursor)
        goto invalid;
    *words = destination >= site ? (int64_t)((destination - site) / 4u)
                                 : -(int64_t)((site - destination) / 4u);
    limit = INT64_C(1) << (bits - 1u);
    if (*words < -limit || *words >= limit) goto invalid;
    return 1;
invalid:
    if (assembler != NULL) assembler->failed = 1;
    return 0;
}

static void condition(hl_a64_assembler *assembler, uint32_t *instruction,
                      const uint8_t *target, unsigned value) {
    int64_t words;
    if (!patch_offset(assembler, instruction, target, 19u, &words)) return;
    *instruction = 0x54000000u | (((uint32_t)words & 0x7ffffu) << 5) | value;
}

static void branch(hl_a64_assembler *assembler, uint32_t *instruction, const uint8_t *target);
static void link(hl_a64_assembler *assembler, uint32_t *instruction, const uint8_t *target);

/* Archive the live exact interval, reusing an existing same-owner record when
 * the ranges overlap or touch. x16 carries the current guest address and is
 * retained in fault_address while x16/x17/x18/x9 execute the bounded reverse
 * scan. The caller has already saved guest x9 and NZCV. */
static void archive_dirty_body(hl_a64_assembler *assembler, uint64_t pc) {
    uint32_t *none, *empty, *different[2], *before, *after, *keep_first, *keep_last,
        *merged, *overflow, *success;
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, UINT32_C(0xb100063f)); /* cmn x17,#1: empty sentinel */
    none = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_str(assembler, 16, CPU, OFFSET_FAULT_ADDRESS);
    hl_a64_ldr(assembler, 16, CPU, OFFSET_DIRTY_COUNT);
    uint8_t *scan = assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0xf100021f)); /* cmp x16,#0 */
    empty = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_subi(assembler, 16, 16, 1);
    hl_a64_addi(assembler, 18, CPU, OFFSET_DIRTY_RECORDS);
    hl_a64_emit32(assembler, UINT32_C(0x8b000000) | (16u << 16) | (5u << 10) |
                                 (18u << 5) | 18u); /* add x18,x18,x16,lsl#5 */
    hl_a64_ldr(assembler, 17, 18, 0);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_VIEW_FIRST);
    hl_a64_emit32(assembler, UINT32_C(0xeb09023f));
    different[0] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, 18, 8);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_VIEW_LAST);
    hl_a64_emit32(assembler, UINT32_C(0xeb09023f));
    different[1] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, 18, 24);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, UINT32_C(0xeb09023f)); /* cmp record_last,active_first */
    before = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_LAST);
    hl_a64_ldr(assembler, 9, 18, 16);
    hl_a64_emit32(assembler, UINT32_C(0xeb09023f)); /* cmp active_last,record_first */
    after = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    hl_a64_ldr(assembler, 17, 18, 16);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, UINT32_C(0xeb11013f)); /* cmp active_first,record_first */
    keep_first = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_str(assembler, 9, 18, 16);
    uint8_t *first_done = assembler->cursor;
    hl_a64_ldr(assembler, 17, 18, 24);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_LAST);
    hl_a64_emit32(assembler, UINT32_C(0xeb11013f)); /* cmp active_last,record_last */
    keep_last = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_str(assembler, 9, 18, 24);
    uint8_t *last_done = assembler->cursor;
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_merged));
    merged = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    uint8_t *next = assembler->cursor;
    uint32_t *again = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *append = assembler->cursor;
    hl_a64_ldr(assembler, 16, CPU, OFFSET_DIRTY_COUNT);
    hl_a64_emit32(assembler, UINT32_C(0xf100421f)); /* cmp count,#16 */
    overflow = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_addi(assembler, 18, CPU, OFFSET_DIRTY_RECORDS);
    hl_a64_emit32(assembler, UINT32_C(0x8b000000) | (16u << 16) | (5u << 10) |
                                 (18u << 5) | 18u);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_VIEW_FIRST);
    hl_a64_str(assembler, 17, 18, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_VIEW_LAST);
    hl_a64_str(assembler, 17, 18, 8);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_str(assembler, 17, 18, 16);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_LAST);
    hl_a64_str(assembler, 17, 18, 24);
    hl_a64_addi(assembler, 16, 16, 1);
    hl_a64_str(assembler, 16, CPU, OFFSET_DIRTY_COUNT);

    uint8_t *done = assembler->cursor;
    hl_a64_emit32(assembler, UINT32_C(0x92800011)); /* movn x17,#0 */
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_movz(assembler, 17, 0, 0);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_LAST);
    hl_a64_ldr(assembler, 16, CPU, OFFSET_FAULT_ADDRESS);
    success = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    uint8_t *overflow_target = assembler->cursor;
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_overflow));
    hl_a64_ldr(assembler, 16, CPU, OFFSET_FAULT_ADDRESS);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, UINT32_C(0xd51b4211));
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_EPOCH, pc);
    uint8_t *resume = assembler->cursor;
    hl_a64_ret(assembler);
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(assembler, none, resume, 0u);
    condition(assembler, empty, append, 0u);
    condition(assembler, different[0], next, 1u);
    condition(assembler, different[1], next, 1u);
    condition(assembler, before, next, 3u); /* record_last < active_first */
    condition(assembler, after, next, 3u);  /* active_last < record_first */
    condition(assembler, keep_first, first_done, 2u);
    condition(assembler, keep_last, last_done, 9u);
    branch(assembler, merged, done);
    branch(assembler, again, scan);
    condition(assembler, overflow, overflow_target, 2u);
    branch(assembler, success, resume);
}

/* Both archive sites of one guarded store share its pc, so the body is emitted
 * once and reached by bl; x30 is stolen and dead at every store guard. */
static void archive_dirty(hl_a64_assembler *assembler, uint64_t pc, uint8_t **archive) {
    if (*archive == NULL) {
        uint32_t *over = (uint32_t *)assembler->cursor;
        hl_a64_emit32(assembler, 0);
        *archive = assembler->cursor;
        archive_dirty_body(assembler, pc);
        if (!hl_a64_assembler_ok(assembler)) return;
        branch(assembler, over, assembler->cursor);
    }
    uint32_t *call = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    if (!hl_a64_assembler_ok(assembler)) return;
    link(assembler, call, *archive);
}

void hl_a64_guard_write_begin(hl_a64_assembler *assembler, uint64_t bytes, uint64_t pc,
                              hl_a64_guard *guard) {
    uint32_t *empty, *not_contiguous, *contiguous, *different_view[2], *above, *below, *safe;
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u); /* mrs x9,nzcv */
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0xCB110210u); /* sub x16,x16,x17: recover guest EA */
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, 0xB100063Fu); /* cmn x17,#1: empty sentinel */
    empty = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    /* Guest contiguity is insufficient: adjacent mappings can belong to
     * distinct projection owners. Only the same bounded view can extend the
     * live exact interval without archiving it first. */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FIRST);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_DIRTY_VIEW_FIRST);
    hl_a64_emit32(assembler, 0xEB12023Fu);
    different_view[0] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_LAST);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_DIRTY_VIEW_LAST);
    hl_a64_emit32(assembler, 0xEB12023Fu);
    different_view[1] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_LAST);
    hl_a64_emit32(assembler, 0xEB09021Fu); /* cmp address,last */
    not_contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *noncontiguous = assembler->cursor;
    above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    /* The view-identity checks above consumed x17 and x18, so recover the end
     * and the live start before comparing them. */
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, 0xEB11025Fu); /* cmp end,first */
    below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    safe = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *disjoint = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(assembler, not_contiguous, noncontiguous, 1u);
    condition(assembler, different_view[0], disjoint, 1u);
    condition(assembler, different_view[1], disjoint, 1u);
    condition(assembler, above, disjoint, 8u);
    condition(assembler, below, disjoint, 3u);
    archive_dirty(assembler, pc, &guard->archive);
    uint8_t *safe_target = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(assembler, empty, safe_target, 0u);
    branch(assembler, contiguous, safe_target);
    branch(assembler, safe, safe_target);
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_reserved));
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0x8B110210u); /* restore projected EA */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u); /* msr nzcv,x17 */
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
}

void hl_a64_guard_written(hl_a64_assembler *assembler, uint64_t bytes) {
    uint32_t *empty, *not_contiguous, *contiguous, *different_view[2], *above, *below, *after_merge, *after_range,
        *after_contiguous;
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u); /* mrs x9,nzcv */
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, 0xCB110210u); /* recover guest EA */
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, 0xB100063Fu); /* cmn x17,#1: empty sentinel */
    empty = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    /* Commit this only after the host store above the emitted guard succeeds.
     * Adjacent addresses merge only under the same projection-view owner. */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FIRST);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_DIRTY_VIEW_FIRST);
    hl_a64_emit32(assembler, 0xEB12023Fu);
    different_view[0] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_LAST);
    hl_a64_ldr(assembler, 18, CPU, OFFSET_DIRTY_VIEW_LAST);
    hl_a64_emit32(assembler, 0xEB12023Fu);
    different_view[1] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_DIRTY_LAST);
    hl_a64_emit32(assembler, 0xEB09021Fu);
    not_contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *noncontiguous = assembler->cursor;
    above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    /* Same recovery as the reservation: the view-identity checks consumed x17
     * and x18, and the union below needs the end and the live start, not the
     * view bounds that overwrote them. */
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, 0xEB11025Fu);
    below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_emit32(assembler, 0xEB11021Fu);
    hl_a64_emit32(assembler, 0x9A913211u);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_emit32(assembler, 0xEB09025Fu);
    hl_a64_emit32(assembler, 0x9A898251u);
    hl_a64_str(assembler, 17, CPU, OFFSET_DIRTY_LAST);
    /* The completed store overlaps the live interval and merged in place. */
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_merged));
    after_merge = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *disjoint = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(assembler, not_contiguous, noncontiguous, 1u);
    condition(assembler, different_view[0], disjoint, 1u);
    condition(assembler, different_view[1], disjoint, 1u);
    condition(assembler, above, disjoint, 8u);
    condition(assembler, below, disjoint, 3u);
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
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(assembler, empty, set, 0u);
    hl_a64_str(assembler, 16, CPU, OFFSET_DIRTY_FIRST);
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_str(assembler, 18, CPU, OFFSET_DIRTY_LAST);
    if (!hl_a64_assembler_ok(assembler)) return;
    branch(assembler, after_merge, assembler->cursor);
    after_range = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *contiguous_target = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    branch(assembler, contiguous, contiguous_target);
    /* An adjacent completed store extends the same live interval. */
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_merged));
    hl_a64_addi(assembler, 18, 16, (unsigned)bytes);
    hl_a64_str(assembler, 18, CPU, OFFSET_DIRTY_LAST);
    /* The interval's view owner, written bit, and executable bit were
     * established by the first successful store. A contiguous continuation
     * changes only the exact interval end. Preserve the completed-store
     * diagnostic without rewriting that invariant metadata. */
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_committed));
    after_contiguous = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    uint8_t *range_done = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    branch(assembler, after_range, range_done);
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
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_dirty_committed));
    uint8_t *restore = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    branch(assembler, after_contiguous, restore);
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
}

static void test(hl_a64_assembler *assembler, uint32_t *instruction,
                 const uint8_t *target, unsigned bit, unsigned reg) {
    int64_t words;
    if (!patch_offset(assembler, instruction, target, 14u, &words)) return;
    *instruction = 0x36000000u | ((bit & 0x20u) << 26) | ((bit & 31u) << 19) |
                   (((uint32_t)words & 0x3fffu) << 5) | (reg & 31u);
}

static void branch(hl_a64_assembler *assembler, uint32_t *instruction, const uint8_t *target) {
    int64_t words;
    if (!patch_offset(assembler, instruction, target, 26u, &words)) return;
    *instruction = 0x14000000u | ((uint32_t)words & 0x03ffffffu);
}

static void link(hl_a64_assembler *assembler, uint32_t *instruction, const uint8_t *target) {
    int64_t words;
    if (!patch_offset(assembler, instruction, target, 26u, &words)) return;
    *instruction = 0x94000000u | ((uint32_t)words & 0x03ffffffu);
}

/* The view payload is immutable for a whole entry, so the scan is a count-driven
 * loop over x30 rather than four unrolled copies of the same twelve words. */
static void read_cache(hl_a64_assembler *assembler, uint64_t bytes, uint32_t **hit) {
    uint32_t *active[1];
    uint32_t *exhausted;
    uint32_t *below;
    uint32_t *above;
    uint32_t *denied;
    uint32_t *selected;
    uint32_t *again;
    uint8_t *loop;
    uint8_t *following;
    uint8_t *resume;
    unsigned active_count = 0;

    hl_a64_addi(assembler, 9, 16, (unsigned)bytes);
    hl_a64_emit32(assembler, 0xEB10013Fu); /* cmp x9,x16: end wrapped */
    active[active_count++] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    /* The entry trampoline already acquired the token and proved the table; a
     * zero count here exhausts the scan on its first pass and fails closed. */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_READ_VALID_COUNT);

    hl_a64_addi(assembler, 30, CPU, OFFSET_READ_VIEWS);
    loop = assembler->cursor;
    hl_a64_emit32(assembler, 0xF1000631u); /* subs x17,x17,#1: slots remaining */
    exhausted = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, 0);
    hl_a64_emit32(assembler, 0xEB12021Fu); /* cmp guest EA,first */
    below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, (int)sizeof(uint64_t));
    hl_a64_emit32(assembler, 0xEB12013Fu); /* cmp end,last */
    above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, 3 * (int)sizeof(uint64_t));
    denied = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, 2 * (int)sizeof(uint64_t));
    hl_a64_emit32(assembler, 0x8B120210u); /* add projected EA */
    selected = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    following = assembler->cursor;
    hl_a64_addi(assembler, 30, 30, 4u * (unsigned)sizeof(uint64_t));
    again = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    resume = assembler->cursor;
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    *hit = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    uint8_t *active_target = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    condition(assembler, active[0], active_target, 3u);
    condition(assembler, exhausted, active_target, 3u);
    condition(assembler, below, following, 3u);
    condition(assembler, above, following, 8u);
    test(assembler, denied, following, 0u, 18u);
    branch(assembler, selected, resume);
    branch(assembler, again, loop);
}

/* A write miss may be an alternation between already projected stack and heap
 * views. Resolve those immutable views inside the trace, but also install the
 * selected view as the active write owner so exact dirty intervals retain the
 * correct projection identity. The slow callback remains authoritative for a
 * view absent from this bounded, generation-qualified cache. */
static void write_cache(hl_a64_assembler *assembler, uint64_t bytes, uint64_t pc,
                        const uint8_t *resume, uint8_t **archive) {
    uint32_t *inactive[1];
    uint32_t *exhausted;
    uint32_t *below;
    uint32_t *above;
    uint32_t *denied;
    uint32_t *chosen;
    uint32_t *again;
    uint8_t *loop;
    uint8_t *following;

    hl_a64_addi(assembler, 9, 16, (unsigned)bytes);
    hl_a64_emit32(assembler, 0xEB10013Fu); /* cmp end,address: wrapped */
    inactive[0] = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    /* Match read_cache: the entry trampoline proved the table for this entry. */
    hl_a64_ldr(assembler, 17, CPU, OFFSET_READ_VALID_COUNT);

    hl_a64_addi(assembler, 30, CPU, OFFSET_READ_VIEWS);
    loop = assembler->cursor;
    hl_a64_emit32(assembler, 0xF1000631u); /* subs x17,x17,#1: slots remaining */
    exhausted = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, 0);
    hl_a64_emit32(assembler, 0xEB12021Fu); /* cmp address,first */
    below = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, (int)sizeof(uint64_t));
    hl_a64_emit32(assembler, 0xEB12013Fu); /* cmp end,last */
    above = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    hl_a64_ldr(assembler, 18, 30, 3 * (int)sizeof(uint64_t));
    denied = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);
    chosen = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    following = assembler->cursor;
    hl_a64_addi(assembler, 30, 30, 4u * (unsigned)sizeof(uint64_t));
    again = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    uint8_t *selected_target = assembler->cursor;
    /* The countdown consumed the slot number the unrolled scan used to
     * materialise, so recover it as read_count - remaining - 1. */
    hl_a64_ldr(assembler, 18, CPU, OFFSET_READ_VALID_COUNT);
    hl_a64_emit32(assembler, UINT32_C(0xcb110252)); /* sub x18,x18,x17 */
    hl_a64_subi(assembler, 9, 18, 1);
    /* archive_dirty uses x9 while scanning records. Preserve the selected
     * read-view slot in cold fault metadata; every actual fault path replaces
     * this field before exposing it to the dispatcher. */
    hl_a64_str(assembler, 9, CPU, OFFSET_FAULT_SIZE);
    archive_dirty(assembler, pc, archive);
    hl_a64_ldr(assembler, 9, CPU, OFFSET_FAULT_SIZE);

    hl_a64_addi(assembler, 18, CPU, OFFSET_READ_VIEWS);
    hl_a64_emit32(assembler, UINT32_C(0x8b000000) | (9u << 16) | (5u << 10) |
                                 (18u << 5) | 18u); /* add x18,x18,x9,lsl#5 */
    hl_a64_ldr(assembler, 17, 18, 0);
    hl_a64_str(assembler, 17, CPU, OFFSET_FIRST);
    hl_a64_ldr(assembler, 17, 18, (int)sizeof(uint64_t));
    hl_a64_str(assembler, 17, CPU, OFFSET_LAST);
    hl_a64_ldr(assembler, 17, 18, 2 * (int)sizeof(uint64_t));
    hl_a64_str(assembler, 17, CPU, OFFSET_DELTA);
    hl_a64_emit32(assembler, UINT32_C(0x8b110210)); /* add projected EA */
    hl_a64_ldr(assembler, 17, 18, 3 * (int)sizeof(uint64_t));
    hl_a64_str(assembler, 17, CPU, OFFSET_PERMISSIONS);
    hl_a64_addi(assembler, 18, CPU, OFFSET_READ_VIEW_PUBLICATION);
    hl_a64_addlsl4(assembler, 18, 18, 9);
    hl_a64_ldr(assembler, 17, 18, 0);
    hl_a64_str(assembler, 17, CPU, OFFSET_WRITE_POLICY);
    hl_a64_ldr(assembler, 17, 18, (int)sizeof(uint64_t));
    hl_a64_str(assembler, 17, CPU, OFFSET_WRITE_INDEX);
    /* A write that the latched window missed but this cache resolved. */
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_guard_fast));
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, UINT32_C(0xd51b4211)); /* msr nzcv,x17 */
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    uint32_t *retry = (uint32_t *)assembler->cursor;
    hl_a64_emit32(assembler, 0);

    uint8_t *miss = assembler->cursor;
    if (!hl_a64_assembler_ok(assembler)) return;
    branch(assembler, chosen, selected_target);
    branch(assembler, retry, resume);
    condition(assembler, inactive[0], miss, 3u);
    condition(assembler, exhausted, miss, 3u);
    condition(assembler, below, following, 3u);
    condition(assembler, above, following, 8u);
    test(assembler, denied, following, 1u, 18u);
    branch(assembler, again, loop);
}

void hl_a64_guard_begin(hl_a64_assembler *assembler, uint64_t bytes, uint32_t required,
                        hl_a64_guard *guard) {
    uint32_t *cache_hit = NULL;
    hl_a64_str(assembler, 9, CPU, 9 * 8);
    hl_a64_emit32(assembler, 0xD53B4209u);
    hl_a64_str(assembler, 9, CPU, OFFSET_FLAGS);
    if (required == HL_A64_PERMISSION_READ) read_cache(assembler, bytes, &cache_hit);
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
    if (required == HL_A64_PERMISSION_READ) branch(assembler, cache_hit, assembler->cursor);
    guard->resume = assembler->cursor;
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_guard_full));
    guard->required = required;
    guard->bytes = bytes;
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
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_guard_full));
    guard->required = required;
    guard->bytes = bytes;
    guard->direct = 1;
}

void hl_a64_guard_finish(hl_a64_assembler *assembler, const hl_a64_guard *guard) {
    /* Placeholder patchers write directly, unlike bounded instruction
     * emission.  A failed begin may therefore leave a placeholder at the
     * one-past-end cursor; never patch a guard after capacity exhaustion. */
    if (!hl_a64_assembler_ok(assembler)) return;
    if (guard == NULL) {
        assembler->failed = 1;
        return;
    }
    if (guard->below == NULL) {
        if (guard->overflow == NULL && guard->above == NULL && guard->permission == NULL &&
            guard->resume == NULL && guard->required == 0 && guard->bytes == 0)
            return; /* Non-faulting PRFM emits no guard. */
        assembler->failed = 1;
        return;
    }
    if (guard->overflow == NULL || guard->above == NULL || guard->permission == NULL ||
        guard->resume == NULL) {
        assembler->failed = 1;
        return;
    }
    uint8_t *miss = assembler->cursor;
    condition(assembler, guard->below, miss, 3);
    condition(assembler, guard->overflow, miss, 3);
    condition(assembler, guard->above, miss, 8);
    test(assembler, guard->permission, miss,
         guard->required == HL_A64_PERMISSION_READ ? 0u : 1u, 17u);
    if (guard->required == HL_A64_PERMISSION_WRITE) {
        uint8_t *archive = guard->archive;
        write_cache(assembler, guard->bytes, guard->pc, guard->resume, &archive);
    } else if (guard->required == HL_A64_PERMISSION_READ && guard->direct) {
        /* Mirror write_cache: a direct read outside the window is recoverable
         * from the same bounded cache instead of leaving to the dispatcher. */
        uint32_t *hit = NULL;
        read_cache(assembler, guard->bytes, &hit);
        if (!hl_a64_assembler_ok(assembler)) return;
        branch(assembler, hit, guard->resume);
    }
    if (!hl_a64_assembler_ok(assembler)) return;
    hl_a64_ldr(assembler, 17, CPU, OFFSET_FLAGS);
    hl_a64_emit32(assembler, 0xD51B4200u | 17u);
    hl_a64_ldr(assembler, 9, CPU, 9 * 8);
    hl_a64_str(assembler, 16, CPU, OFFSET_FAULT_ADDRESS);
    hl_a64_movconst(assembler, 17, guard->required);
    hl_a64_str(assembler, 17, CPU, OFFSET_FAULT_ACCESS);
    hl_a64_movconst(assembler, 17, guard->bytes);
    hl_a64_str(assembler, 17, CPU, OFFSET_FAULT_SIZE);
    hl_a64_movconst(assembler, 17, guard->completed);
    hl_a64_str(assembler, 17, CPU, OFFSET_FAULT_COMPLETED);
    diagnostic_increment(assembler, (int)offsetof(hl_native_aarch64_cpu, diagnostic_guard_fallback));
    /* Only an operand-resolution fallback needs the executing cache identity
     * to refund the uncompleted suffix.  Publish it on that cold path instead
     * of charging every trace and chained entry. */
    hl_a64_stub_publish_execution_identity(assembler);
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_FALLBACK, guard->pc);
}
