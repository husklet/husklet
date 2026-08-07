#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"
#include "../../../executor.h"

static void branch(uint32_t *instruction, uint32_t *target, unsigned condition) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x54000000) | ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | condition;
}

static void test(uint32_t *instruction, uint32_t *target, unsigned bit) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x36000000) | (bit & 0x20u) << 26 | (bit & 31u) << 19 |
                   ((uint32_t)distance & UINT32_C(0x3fff)) << 5 | 17u;
}

static void zero_test(uint32_t *instruction, uint32_t *target, unsigned source) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0xb4000000) | ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | source;
}

#define HL_X86_READ_VIEWS 4u
#define HL_X86_WRITE_CACHE_WORDS 85u

/* The empty-journal sentinel is UINT64_MAX.  emit_constant reaches it with a
 * movz and three movk; MOVN writes the same all-ones pattern in one word. */
static uint32_t ones_word(unsigned destination) {
    return UINT32_C(0x92800000) | destination;
}

/* LDP and STP reach only +-504 bytes from their base, and every projection and
 * journal field sits far past that from the CPU record in x28.  Anchoring a
 * window on memory_first brings all of them -- the projection quadruple, the
 * dirty interval, its owner and the record count -- inside one base register's
 * range, so the write cache can move them in pairs. */
#define HL_X86_WINDOW offsetof(hl_native_x86_64_cpu, memory_first)
#define HL_X86_WINDOW_BASE 18u

_Static_assert(offsetof(hl_native_x86_64_cpu, dirty_count) - HL_X86_WINDOW <= 504u,
               "the write cache window must reach every field it pairs");
_Static_assert(offsetof(hl_native_x86_64_cpu, memory_last) ==
                       offsetof(hl_native_x86_64_cpu, memory_first) + 8u &&
                   offsetof(hl_native_x86_64_cpu, memory_permissions) ==
                       offsetof(hl_native_x86_64_cpu, memory_delta) + 8u &&
                   offsetof(hl_native_x86_64_cpu, dirty_view_last) ==
                       offsetof(hl_native_x86_64_cpu, dirty_view_first) + 8u &&
                   offsetof(hl_native_x86_64_cpu, dirty_last) ==
                       offsetof(hl_native_x86_64_cpu, dirty_first) + 8u,
               "the write cache moves these fields in pairs");

/* memory_first is the lowest field in the window, so every displacement is
 * non-negative and fits the scaled twelve-bit LDR form and the seven-bit LDP
 * form alike. */
static uint32_t window_word(size_t field) {
    return (uint32_t)((field - HL_X86_WINDOW) / 8u) & UINT32_C(0x7f);
}

static uint32_t window_load(unsigned destination, size_t field) {
    return UINT32_C(0xf9400000) | window_word(field) << 10 | HL_X86_WINDOW_BASE << 5 | destination;
}

static uint32_t window_store(unsigned source, size_t field) {
    return UINT32_C(0xf9000000) | window_word(field) << 10 | HL_X86_WINDOW_BASE << 5 | source;
}

static uint32_t pair_load(unsigned first, unsigned second, unsigned base, uint32_t slot) {
    return UINT32_C(0xa9400000) | (slot & UINT32_C(0x7f)) << 15 | second << 10 | base << 5 | first;
}

static uint32_t pair_store(unsigned first, unsigned second, unsigned base, uint32_t slot) {
    return UINT32_C(0xa9000000) | (slot & UINT32_C(0x7f)) << 15 | second << 10 | base << 5 | first;
}

/* The entry trampolines bound the published count against this capacity as a
 * literal, so it must keep tracking the record. */
_Static_assert(HL_X86_READ_VIEWS == sizeof(((hl_native_x86_64_cpu *)0)->read_views) /
                                        sizeof(((hl_native_x86_64_cpu *)0)->read_views[0]),
               "x86 entry trampolines validate read_count against a literal capacity of four");

static void jump(uint32_t *instruction, uint32_t *target) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x14000000) | ((uint32_t)distance & UINT32_C(0x03ffffff));
}

/* Token, incarnation and view count are frozen for a whole entry into generated
 * code, so the trampoline validates them once and leaves the usable view count
 * in x29 (zero when the table cannot be trusted).  The scan is a count-driven
 * loop: a pointer and a countdown cost one stp/ldp of x19 and x22, which the
 * write cache's prologue already spills, and that beats four unrolled copies. */
uint32_t hl_x86_read_cache_words(unsigned width) {
    return 21u + (width == 32u ? 1u : 0u);
}

void hl_x86_emit_read_cache(uint32_t *words, uint32_t *cursor, unsigned width,
                            unsigned destination, int vector, uint32_t **hit) {
    uint32_t *overflow, *empty, *below, *above, *permission, *repeat;
    words[(*cursor)++] = UINT32_C(0xa9bf5bf3); /* stp x19,x22,[sp,#-16]! */
    words[(*cursor)++] = UINT32_C(0xb1000000) | width << 10 | 16u << 5 | 18u; /* adds x18,x16,#width */
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0x91000393) |
                         (uint32_t)offsetof(hl_native_x86_64_cpu, read_views) << 10; /* x19 = views */
    words[(*cursor)++] = UINT32_C(0xaa1d03f6); /* x22 = validated count */
    uint32_t *loop = &words[*cursor];
    empty = &words[(*cursor)++];
    words[(*cursor)++] = pair_load(20, 17, 19, 0); /* ldp x20,x17,[x19]: view first,last */
    words[(*cursor)++] = UINT32_C(0xeb14021f); /* cmp x16,x20 */
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    above = &words[(*cursor)++];
    words[(*cursor)++] = pair_load(20, 17, 19, 2); /* ldp x20,x17,[x19,#16]: delta,permissions */
    permission = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0x8b140211); /* add x17,x16,x20 */
    words[(*cursor)++] = vector
        ? ((width == 4u ? UINT32_C(0xbd400220) :
            width == 8u ? UINT32_C(0xfd400220) : UINT32_C(0x3dc00220)) | destination)
        : ((width == 1u ? UINT32_C(0x39400220) :
            width == 2u ? UINT32_C(0x79400220) :
            width == 4u ? UINT32_C(0xb9400220) : UINT32_C(0xf9400220)) | destination);
    if (vector && width == 32u)
        words[(*cursor)++] = UINT32_C(0x3dc00620) | (destination + 1u); /* ldr qD+1,[x17,#16] */
    words[(*cursor)++] = UINT32_C(0xa8c15bf3); /* ldp x19,x22,[sp],#16 */
    *hit = &words[(*cursor)++];
    uint32_t *following = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0x91008273); /* add x19,x19,#32 */
    words[(*cursor)++] = UINT32_C(0xd10006d6); /* sub x22,x22,#1 */
    repeat = &words[(*cursor)++];
    uint32_t *active = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0xa8c15bf3); /* ldp x19,x22,[sp],#16 */
    jump(repeat, loop);
    branch(overflow, active, 2u); /* address plus width wrapped */
    zero_test(empty, active, 22u); /* no further published view */
    branch(below, following, 3u); /* address below first */
    branch(above, following, 8u); /* end above last */
    test(permission, following, 0u);
}

void hl_x86_emit_vector_upper_load(uint32_t *words, uint32_t *cursor,
                                   unsigned destination, unsigned guest) {
    size_t offset = offsetof(hl_native_x86_64_cpu, vector_upper) + guest * 16u;
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)offset << 10 | 28u << 5 | 20u;
    words[(*cursor)++] = UINT32_C(0x3dc00280) | destination; /* ldr qD,[x20] */
}

void hl_x86_emit_vector_upper_store(uint32_t *words, uint32_t *cursor,
                                    unsigned source, unsigned guest) {
    size_t offset = offsetof(hl_native_x86_64_cpu, vector_upper) + guest * 16u;
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)offset << 10 | 28u << 5 | 20u;
    words[(*cursor)++] = UINT32_C(0x3d800280) | source; /* str qS,[x20] */
}

void hl_x86_emit_vector_upper_zero(uint32_t *words, uint32_t *cursor, unsigned guest) {
    size_t offset = offsetof(hl_native_x86_64_cpu, vector_upper) + guest * 16u;
    words[(*cursor)++] = store_word(31u, offset);
    words[(*cursor)++] = store_word(31u, offset + 8u);
}

void hl_x86_patch_read_hit(uint32_t *hit, uint32_t *target) { jump(hit, target); }

/* Selects an already-published writable projection without returning to the
 * dispatcher.  A view transition archives the old exact dirty owner before
 * changing active projection state; a full journal falls through to the
 * ordinary miss path before any guest byte is changed. */
static void emit_write_cache(uint32_t *words, uint32_t *cursor, unsigned width) {
    uint32_t *overflow;
    words[(*cursor)++] = UINT32_C(0xa9be53f3); /* preserve caller temporaries */
    words[(*cursor)++] = UINT32_C(0xa9015bf5);
    words[(*cursor)++] = UINT32_C(0xb1000000) | width << 10 | 16u << 5 | 18u;
    words[(*cursor)++] = UINT32_C(0xeb10025f); /* cmp end,address */
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0x91000013) |
                         (uint32_t)offsetof(hl_native_x86_64_cpu, read_views) << 10 |
                         28u << 5; /* x19 = first published view */
    words[(*cursor)++] = UINT32_C(0xaa1d03f6); /* x22 = validated count */
    uint32_t *loop = &words[*cursor];
    uint32_t *none = &words[(*cursor)++];
    words[(*cursor)++] = pair_load(20, 21, 19, 0); /* ldp x20,x21,[x19]: view first,last */
    words[(*cursor)++] = UINT32_C(0xeb14021f);
    uint32_t *below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xeb15025f);
    uint32_t *above = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9400e71); /* ldr x17,[x19,#24] */
    uint32_t *denied = &words[(*cursor)++];
    uint32_t *selected = &words[(*cursor)++];
    uint32_t *next = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0x91008273); /* add x19,x19,#32 */
    words[(*cursor)++] = UINT32_C(0xd10006d6); /* sub x22,x22,#1 */
    uint32_t *again = &words[(*cursor)++];
    uint32_t *selected_target = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0xaa1303f6); /* preserve selected view in x22 */
    /* The guest end address in x18 has done its work; every caller recomputes
     * it after the cache, so x18 becomes the window base from here on. */
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)HL_X86_WINDOW << 10 | 28u << 5 |
                         HL_X86_WINDOW_BASE;
    words[(*cursor)++] = pair_load(17, 19, HL_X86_WINDOW_BASE,
                                   window_word(offsetof(hl_native_x86_64_cpu, memory_first)));
    words[(*cursor)++] = UINT32_C(0xeb14023f); /* cmp x17,x20 */
    uint32_t *different_first = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xeb15027f); /* cmp x19,x21 */
    uint32_t *different_last = &words[(*cursor)++];
    uint32_t *already_active = &words[(*cursor)++];

    uint32_t *archive = &words[*cursor];
    words[(*cursor)++] = window_load(17, offsetof(hl_native_x86_64_cpu, dirty_first));
    words[(*cursor)++] = UINT32_C(0xb100063f); /* cmn first,#1: UINT64_MAX means empty */
    uint32_t *empty = &words[(*cursor)++];
    words[(*cursor)++] = window_load(21, offsetof(hl_native_x86_64_cpu, dirty_count));
    words[(*cursor)++] = UINT32_C(0x91000013) |
                         (uint32_t)offsetof(hl_native_x86_64_cpu, dirty_records) << 10 |
                         28u << 5; /* x19 = first dirty record */
    uint32_t *scan = &words[*cursor];
    uint32_t *append = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9400271); /* ldr x17,[x19] */
    words[(*cursor)++] = window_load(20, offsetof(hl_native_x86_64_cpu, dirty_view_first));
    words[(*cursor)++] = UINT32_C(0xeb14023f); /* cmp x17,x20 */
    uint32_t *next_owner_first = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9400671); /* ldr x17,[x19,#8] */
    words[(*cursor)++] = window_load(20, offsetof(hl_native_x86_64_cpu, dirty_view_last));
    words[(*cursor)++] = UINT32_C(0xeb14023f); /* cmp x17,x20 */
    uint32_t *next_owner_last = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9400a71); /* ldr x17,[x19,#16]: recorded first */
    words[(*cursor)++] = window_load(20, offsetof(hl_native_x86_64_cpu, dirty_last));
    words[(*cursor)++] = UINT32_C(0xeb14023f); /* cmp recorded first,current last */
    uint32_t *next_above = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9400e71); /* ldr x17,[x19,#24]: recorded last */
    words[(*cursor)++] = window_load(20, offsetof(hl_native_x86_64_cpu, dirty_first));
    words[(*cursor)++] = UINT32_C(0xeb14023f); /* cmp recorded last,current first */
    uint32_t *next_below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9400a71); /* recorded first */
    words[(*cursor)++] = UINT32_C(0xeb11029f); /* cmp current first,recorded first */
    uint32_t *keep_first = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9000a74); /* str current first,[x19,#16] */
    uint32_t *first_done = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0xf9400e71); /* recorded last */
    words[(*cursor)++] = window_load(20, offsetof(hl_native_x86_64_cpu, dirty_last));
    words[(*cursor)++] = UINT32_C(0xeb11029f); /* cmp current last,recorded last */
    uint32_t *keep_last = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xf9000e74); /* str current last,[x19,#24] */
    uint32_t *merged = &words[*cursor];
    words[(*cursor)++] = ones_word(20);
    words[(*cursor)++] = pair_store(20, 31, HL_X86_WINDOW_BASE,
                                    window_word(offsetof(hl_native_x86_64_cpu, dirty_first)));
    uint32_t *merged_install = &words[(*cursor)++];
    uint32_t *scan_next = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0x91008273); /* add record,record,#32 */
    words[(*cursor)++] = UINT32_C(0xd10006b5); /* sub remaining,remaining,#1 */
    uint32_t *scan_again = &words[(*cursor)++];
    uint32_t *append_target = &words[*cursor];
    words[(*cursor)++] = window_load(17, offsetof(hl_native_x86_64_cpu, dirty_count));
    words[(*cursor)++] = UINT32_C(0xf100423f); /* cmp count,#16 */
    uint32_t *full = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0x91000013) |
                         (uint32_t)offsetof(hl_native_x86_64_cpu, dirty_records) << 10 |
                         28u << 5; /* add x19,cpu,#records */
    words[(*cursor)++] = UINT32_C(0xd37bea35); /* lsl x21,x17,#5 */
    words[(*cursor)++] = UINT32_C(0x8b150273); /* add x19,x19,x21 */
    words[(*cursor)++] = pair_load(20, 21, HL_X86_WINDOW_BASE,
                                   window_word(offsetof(hl_native_x86_64_cpu, dirty_view_first)));
    words[(*cursor)++] = UINT32_C(0xa9005674); /* stp x20,x21,[x19] */
    words[(*cursor)++] = pair_load(20, 21, HL_X86_WINDOW_BASE,
                                   window_word(offsetof(hl_native_x86_64_cpu, dirty_first)));
    words[(*cursor)++] = UINT32_C(0xa9015674); /* stp x20,x21,[x19,#16] */
    words[(*cursor)++] = UINT32_C(0x91000631); /* add x17,x17,#1 */
    words[(*cursor)++] = window_store(17, offsetof(hl_native_x86_64_cpu, dirty_count));
    words[(*cursor)++] = ones_word(20);
    words[(*cursor)++] = pair_store(20, 31, HL_X86_WINDOW_BASE,
                                    window_word(offsetof(hl_native_x86_64_cpu, dirty_first)));
    uint32_t *install = &words[*cursor];
    words[(*cursor)++] = pair_load(20, 21, 22, 0); /* view first,last */
    words[(*cursor)++] = pair_store(20, 21, HL_X86_WINDOW_BASE,
                                    window_word(offsetof(hl_native_x86_64_cpu, memory_first)));
    words[(*cursor)++] = pair_load(20, 21, 22, 2); /* view delta,permissions */
    words[(*cursor)++] = pair_store(20, 21, HL_X86_WINDOW_BASE,
                                    window_word(offsetof(hl_native_x86_64_cpu, memory_delta)));
    uint32_t *installed = &words[(*cursor)++];
    uint32_t *active = &words[*cursor];
    words[(*cursor)++] = UINT32_C(0xa9415bf5);
    words[(*cursor)++] = UINT32_C(0xa8c253f3);

    branch(overflow, active, 3u);
    *none = UINT32_C(0xb4000000) |
            ((uint32_t)(active - none) & UINT32_C(0x7ffff)) << 5 | 22u;
    branch(below, next, 3u);
    branch(above, next, 8u);
    test(denied, next, 1u);
    jump(selected, selected_target);
    jump(again, loop);
    branch(different_first, archive, 1u);
    branch(different_last, archive, 1u);
    jump(already_active, active);
    branch(empty, install, 0u);
    *append = UINT32_C(0xb4000000) |
              ((uint32_t)(append_target - append) & UINT32_C(0x7ffff)) << 5 | 21u;
    branch(next_owner_first, scan_next, 1u);
    branch(next_owner_last, scan_next, 1u);
    branch(next_above, scan_next, 8u);
    branch(next_below, scan_next, 3u);
    branch(keep_first, first_done, 2u);
    branch(keep_last, merged, 9u);
    jump(merged_install, install);
    jump(scan_again, scan);
    branch(full, active, 2u);
    jump(installed, active);
}

void hl_x86_emit_dirty(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, dirty_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
    words[(*cursor)++] = UINT32_C(0x9a913211); /* csel x17,x16,x17,lo */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_first));
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, dirty_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    words[(*cursor)++] = UINT32_C(0x9a918251); /* csel x17,x18,x17,hi */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_last));
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_view_first));
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_view_last));
}

uint32_t hl_x86_load_words(const instruction *item) {
    uint32_t access_width = item->load_width != 0u ? item->load_width : item->width;
    uint32_t words = hl_x86_address_words(item) - 1u + 23u + (item->live_chain != 0u ? 19u : 0u) +
                     item->memory_write +
                     constant_words(access_width) + constant_words(item->pc);
    if (item->memory_write == 0u) words += hl_x86_read_cache_words(access_width);
    else words += HL_X86_WRITE_CACHE_WORDS;

    if (item->load_width != 0u && item->signed_extend != 0u && item->width == 2u) ++words;

    return item->conditional != 0u ? words - 1u + hl_x86_cmov_words(item) : words;
}

void hl_x86_emit_load(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint32_t *permission_write = NULL;
    uint32_t *skip;
    uint32_t *cache_hit;
    uint32_t access_width = item->load_width != 0u ? item->load_width : item->width;
    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    if (item->memory_write == 0u)
        hl_x86_emit_read_cache(words, cursor, access_width, 18u, 0, &cache_hit);
    else
        emit_write_cache(words, cursor, access_width);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f);
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | access_width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f);
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    if (item->memory_write != 0u) permission_write = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211);
    words[(*cursor)++] = access_width == 1u ? UINT32_C(0x39400232) :
                           access_width == 2u ? UINT32_C(0x79400232) :
                           access_width == 4u ? UINT32_C(0xb9400232) : UINT32_C(0xf9400232);
    skip = &words[(*cursor)++];
    uint32_t *miss = &words[*cursor];
    branch(below, miss, 3u);
    branch(overflow, miss, 2u);
    branch(above, miss, 8u);
    test(permission, miss, 0u);
    if (permission_write != NULL) test(permission_write, miss, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = item->memory_write != 0u ? UINT32_C(0xd2800071) : UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17, access_width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(skip, &words[*cursor], 14u);
    if (item->memory_write == 0u) hl_x86_patch_read_hit(cache_hit, &words[*cursor]);
    if (item->conditional != 0u)
        hl_x86_emit_cmov(words, cursor, item, 18u);
    else if (item->load_width != 0u) {
        unsigned source = 18u;

        if (item->signed_extend != 0u) {
            unsigned target = item->width == 2u ? 17u : item->destination;
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x93400000) : UINT32_C(0x13000000)) |
                                   (access_width == 1u ? UINT32_C(0x1c00) :
                                                        access_width == 2u ? UINT32_C(0x3c00) :
                                                                           UINT32_C(0x7c00)) |
                                   source << 5 | target;
            source = target;
        }
        if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | source << 5 | item->destination;
        else if (item->signed_extend == 0u)
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                   source << 16 | item->destination;
    }
    else if (item->width == 1u)
        words[(*cursor)++] = (item->destination_high != 0u ? UINT32_C(0xb3781c00) :
                                                               UINT32_C(0xb3401c00)) |
                             18u << 5 | item->destination;
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                             18u << 16 | item->destination;
}

uint32_t hl_x86_store_words(const instruction *item) {
    return hl_x86_address_words(item) - 1u + (item->source_high == 8u ? 1u : 0u) +
           41u + HL_X86_WRITE_CACHE_WORDS + (item->live_chain != 0u ? 19u : 0u) + constant_words(item->pc) +
           (item->has_immediate != 0u ? constant_words(item->operand_immediate) : 0u);
}

void hl_x86_emit_store(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint32_t *skip;
    unsigned source = item->source;
    if (item->has_immediate != 0u) source = 20u;
    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    emit_write_cache(words, cursor, item->width);
    if (item->has_immediate != 0u)
        emit_constant(words, cursor, 20u, item->operand_immediate);
    /* x18 carries the end-of-access address through the bounds checks below, so the
     * high-byte source must land in x20 instead. */
    if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 20u; /* ubfx w20,wS,#8,#8 */
        source = 20u;
    }
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f);
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f);
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211);
    words[(*cursor)++] = (item->width == 1u ? UINT32_C(0x39000220) :
                           item->width == 2u ? UINT32_C(0x79000220) :
                           item->width == 4u ? UINT32_C(0xb9000220) : UINT32_C(0xf9000220)) |
                          source;
    words[(*cursor)++] = UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
    hl_x86_emit_dirty(words, cursor);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
    words[(*cursor)++] = UINT32_C(0xaa120231); /* orr x17,x17,x18: sticky permission latch */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    skip = &words[(*cursor)++];
    uint32_t *miss = &words[*cursor];
    branch(below, miss, 3u);
    branch(overflow, miss, 2u);
    branch(above, miss, 8u);
    test(permission, miss, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800051);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17, item->width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(skip, &words[*cursor], 14u);
}

/* Memory XCHG is implicitly locked.  All fault and alignment decisions precede
 * SWPAL, and publication follows it, so neither fallback nor a guest fault can
 * expose a partial architectural mutation. */
uint32_t hl_x86_xchg_words(const instruction *item) {
    return hl_x86_address_words(item) - 1u + HL_X86_WRITE_CACHE_WORDS + 60u + constant_words(item->width) +
           2u * constant_words(item->pc) + (item->source_high == 8u ? 1u : 0u) +
           (item->live_chain != 0u ? 38u : 0u);
}

void hl_x86_emit_xchg(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below, *overflow, *above, *readable, *writable, *unaligned, *done;
    unsigned source = item->source;
    hl_x86_emit_address(words, cursor, item);
    --*cursor; /* retain the EA in x16 */
    emit_write_cache(words, cursor, item->width);
    if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 20u; /* ubfx w20,wS,#8,#8 */
        source = 20u;
    }
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    readable = &words[(*cursor)++];
    writable = &words[(*cursor)++];
    if (item->width != 1u) {
        emit_constant(words, cursor, 19u, item->width - 1u);
        words[(*cursor)++] = UINT32_C(0xea00001f) | 19u << 16 | 16u << 5; /* tst x16,x19 */
        unaligned = &words[(*cursor)++];
    } else unaligned = NULL;
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211); /* add x17,x16,x17 */
    {
        uint32_t size = item->width == 8u ? UINT32_C(0xc0000000) :
                        item->width == 4u ? UINT32_C(0x80000000) :
                        item->width == 2u ? UINT32_C(0x40000000) : 0u;
        words[(*cursor)++] = (UINT32_C(0xb8e08000) & UINT32_C(0x3fffffff)) | size |
                             source << 16 | 17u << 5 | 18u; /* swpal old,[x17] */
    }
    if (item->width == 1u)
        words[(*cursor)++] = (item->destination_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                             18u << 5 | item->destination;
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                             18u << 16 | item->destination;
    words[(*cursor)++] = UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
    /* SWPAL returned the pre-image in x18; restore the validated guest end
     * before publishing the exact dirty interval. */
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    hl_x86_emit_dirty(words, cursor);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
    words[(*cursor)++] = UINT32_C(0xaa120231);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    done = &words[(*cursor)++];

    uint32_t *fault = &words[*cursor];
    branch(below, fault, 3u); branch(overflow, fault, 2u); branch(above, fault, 8u);
    test(readable, fault, 0u); test(writable, fault, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800071); /* read|write */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17u, item->width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071); /* fallback */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);

    uint32_t *fallback = &words[*cursor];
    if (unaligned != NULL) branch(unaligned, fallback, 1u); /* b.ne */
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    emit_constant(words, cursor, 17u, HL_NATIVE_EXIT_FALLBACK);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(done, &words[*cursor], 14u);
}

int hl_x86_decode_xadd(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       uint8_t lock, size_t start, size_t *cursor) {
    uint8_t modrm, raw;
    if (*cursor >= request->guest_size || *cursor - start >= 15u) {
        *cursor = start; block->status = HL_X86_A64_TRUNCATED;
        block->exit = HL_X86_A64_INTERPRETER; return 0;
    }
    modrm = request->guest_bytes[*cursor]; raw = (modrm >> 3) & 7u;
    item->operation = OP_XADD;
    item->source = (uint8_t)(raw | ((rex & 4u) << 1));
    item->width = opcode == 0xc0u ? 1u : (rex & 8u) ? 8u : operand_16 ? 2u : 4u;
    item->condition = lock;
    if (opcode == 0xc0u && rex == 0u && raw >= 4u) {
        item->source = (uint8_t)(raw - 4u); item->source_high = 8u;
    }
    if ((modrm >> 6) == 3u) {
        uint8_t rm = modrm & 7u;
        if (lock) {
            *cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER; return 0;
        }
        ++*cursor; item->destination = (uint8_t)(rm | ((rex & 1u) << 3));
        if (opcode == 0xc0u && rex == 0u && rm >= 4u) {
            item->destination = (uint8_t)(rm - 4u); item->destination_high = 1u;
        }
        return 1;
    }
    if ((request->flags & HL_X86_A64_LSE) == 0u) {
        *cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
        block->exit = HL_X86_A64_INTERPRETER; return 0;
    }
    if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
    item->operation = OP_XADD; item->memory_operand = 1u; item->memory_write = 1u;
    item->source = (uint8_t)(raw | ((rex & 4u) << 1));
    if (opcode == 0xc0u && rex == 0u && raw >= 4u) {
        item->source = (uint8_t)(raw - 4u); item->source_high = 8u;
    }
    item->width = opcode == 0xc0u ? 1u : (rex & 8u) ? 8u : operand_16 ? 2u : 4u;
    item->condition = lock;
    return 1;
}

void hl_x86_emit_xadd(uint32_t *words, uint32_t *cursor, const instruction *item) {
    instruction add = *item;
    if (!item->memory_operand) {
        words[(*cursor)++] = item->width == 1u ?
            (item->destination_high ? UINT32_C(0xd3483c00) : UINT32_C(0xd3401c00)) |
            item->destination << 5 | 19u :
            (item->width == 8u ? UINT32_C(0xaa0003f3) : UINT32_C(0x2a0003f3)) |
            item->destination << 16;
        words[(*cursor)++] = item->width == 1u ?
            (item->source_high ? UINT32_C(0xd3483c00) : UINT32_C(0xd3401c00)) |
            item->source << 5 | 21u :
            (item->width == 8u ? UINT32_C(0xaa0003f5) : UINT32_C(0x2a0003f5)) |
            item->source << 16;
        words[(*cursor)++] = UINT32_C(0xaa1303f9); /* preserve pre-image in x25 */
        add.destination = 19u; add.source = 21u; add.destination_high = 0u; add.source_high = 0u;
        add.memory_operand = 0u; add.memory_write = 0u; add.alu_kind = 0u; add.flags_only = 1u;
        hl_x86_emit_alu(words, cursor, &add); /* result remains in x18 */
        if (item->width == 1u)
            words[(*cursor)++] = (item->source_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                                 25u << 5 | item->source;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 25u << 5 | item->source;
        else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 25u << 16 | item->source;
        if (item->width == 1u)
            words[(*cursor)++] = (item->destination_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                                 18u << 5 | item->destination;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
        else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 18u << 16 | item->destination;
        return;
    }
    {
        uint32_t *below, *overflow, *above, *readable, *writable, *unaligned, *done;
        unsigned source = item->source;
        hl_x86_emit_address(words, cursor, item); --*cursor;
        emit_write_cache(words, cursor, item->width);
        if (item->source_high) {
            words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 21u;
            source = 21u;
        } else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003f5) : UINT32_C(0x2a0003f5)) |
                                 source << 16;
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
        words[(*cursor)++] = UINT32_C(0xeb11021f); below = &words[(*cursor)++];
        words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
        overflow = &words[(*cursor)++];
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
        words[(*cursor)++] = UINT32_C(0xeb11025f); above = &words[(*cursor)++];
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
        readable = &words[(*cursor)++]; writable = &words[(*cursor)++];
        if (item->width != 1u) {
            emit_constant(words, cursor, 19u, item->width - 1u);
            words[(*cursor)++] = UINT32_C(0xea00001f) | 19u << 16 | 16u << 5;
            unaligned = &words[(*cursor)++];
        } else unaligned = NULL;
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
        words[(*cursor)++] = UINT32_C(0x8b110211);
        words[(*cursor)++] = UINT32_C(0xaa1103f8); /* preserve admitted host EA across flag lowering */
        {
            uint32_t size = item->width == 8u ? UINT32_C(0xc0000000) :
                            item->width == 4u ? UINT32_C(0x80000000) :
                            item->width == 2u ? UINT32_C(0x40000000) : 0u;
            words[(*cursor)++] = UINT32_C(0x38e00000) | size | source << 16 | 17u << 5 | 19u;
        }
        add.destination = 19u; add.source = 21u; add.destination_high = 0u; add.source_high = 0u;
        add.memory_operand = 0u; add.memory_write = 0u; add.alu_kind = 0u; add.flags_only = 1u;
        hl_x86_emit_alu(words, cursor, &add);
        if (item->width == 1u)
            words[(*cursor)++] = (item->source_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                                 19u << 5 | item->source;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 19u << 5 | item->source;
        else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 19u << 16 | item->source;
        words[(*cursor)++] = UINT32_C(0xd2800031);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
        words[(*cursor)++] = UINT32_C(0xcb110310) | 24u << 5;
        words[(*cursor)++] = UINT32_C(0x91000012) | (uint32_t)item->width << 10 | 16u << 5;
        hl_x86_emit_dirty(words, cursor);
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
        words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
        words[(*cursor)++] = UINT32_C(0xaa120231);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
        done = &words[(*cursor)++];
        {
            uint32_t *fault = &words[*cursor];
            branch(below, fault, 3u); branch(overflow, fault, 2u); branch(above, fault, 8u);
            test(readable, fault, 0u); test(writable, fault, 1u);
            words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
            words[(*cursor)++] = UINT32_C(0xd2800071);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
            emit_constant(words, cursor, 17u, item->width);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
            emit_constant(words, cursor, 17u, item->pc);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
            words[(*cursor)++] = UINT32_C(0xd2800071);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
            if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
            words[(*cursor)++] = UINT32_C(0xd65f03c0);
        }
        {
            uint32_t *fallback = &words[*cursor];
            if (unaligned) branch(unaligned, fallback, 1u);
            emit_constant(words, cursor, 17u, item->pc);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
            emit_constant(words, cursor, 17u, HL_NATIVE_EXIT_FALLBACK);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
            if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
            words[(*cursor)++] = UINT32_C(0xd65f03c0);
        }
        branch(done, &words[*cursor], 14u);
        return;
    }
}

uint32_t hl_x86_xadd_words(const instruction *item) {
    uint32_t scratch[512], cursor = 0;
    hl_x86_emit_xadd(scratch, &cursor, item);
    return cursor;
}

uint32_t hl_x86_cmpxchg_words(const instruction *item) {
    uint32_t scratch[512], cursor = 0;
    hl_x86_emit_cmpxchg(scratch, &cursor, item);
    return cursor;
}

/* Locked memory CMPXCHG.  The complete read/write proof and natural-alignment
 * test happen before CASAL.  CASAL itself is the sole possible mutation; the
 * write publications are reached only when its returned pre-image matched
 * the accumulator. */
void hl_x86_emit_cmpxchg(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below, *overflow, *above, *readable, *writable, *unaligned, *done, *published, *matched;
    unsigned source = item->source;
    instruction compare = *item;

    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    emit_write_cache(words, cursor, item->width);
    if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 21u;
        source = 21u;
    }
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    readable = &words[(*cursor)++]; writable = &words[(*cursor)++];
    if (item->width != 1u) {
        emit_constant(words, cursor, 19u, item->width - 1u);
        words[(*cursor)++] = UINT32_C(0xea00001f) | 19u << 16 | 16u << 5;
        unaligned = &words[(*cursor)++];
    } else unaligned = NULL;
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211);
    words[(*cursor)++] = UINT32_C(0xaa1003f8); /* preserve validated guest EA in x24 */
    /* Preserve the replacement before x20 is loaded with the comparand. */
    if (source != 21u)
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003f5) : UINT32_C(0x2a0003f5)) |
                             source << 16; /* mov x/w21, source */
    if (item->width == 1u)
        words[(*cursor)++] = UINT32_C(0xd3401c14); /* ubfx x20,x0,#0,#8 */
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xd3403c14); /* ubfx x20,x0,#0,#16 */
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003f4) : UINT32_C(0x2a0003f4));
    {
        uint32_t base = item->width == 8u ? UINT32_C(0xc8e0fc00) :
                        item->width == 4u ? UINT32_C(0x88e0fc00) :
                        item->width == 2u ? UINT32_C(0x48e0fc00) : UINT32_C(0x08e0fc00);
        words[(*cursor)++] = base | 20u << 16 | 17u << 5 | 21u; /* casal expected,new,[host] */
    }
    words[(*cursor)++] = UINT32_C(0xaa1403f9); /* mov x25,x20: returned old value */
    compare.memory_operand = 0u; compare.memory_write = 0u;
    compare.destination = 0u; compare.destination_high = 0u;
    compare.source = 25u; compare.source_high = 0u;
    compare.has_immediate = 0u; compare.alu_kind = 7u; compare.flags_only = 1u;
    compare.preserve_carry = 0u; compare.preserve_flags = 0u; compare.unary_neg = 0u;
    hl_x86_emit_alu(words, cursor, &compare);
    matched = &words[(*cursor)++]; /* b.eq publication */
    if (item->width == 1u)
        words[(*cursor)++] = UINT32_C(0xb3401f20) | 25u << 5; /* bfi x0,x25,#0,#8 */
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403f20) | 25u << 5; /* bfi x0,x25,#0,#16 */
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                             25u << 16;
    done = &words[(*cursor)++];
    branch(matched, &words[*cursor], 0u);
    words[(*cursor)++] = UINT32_C(0xaa1803f0); /* restore guest EA */
    words[(*cursor)++] = UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    hl_x86_emit_dirty(words, cursor);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
    words[(*cursor)++] = UINT32_C(0xaa120231);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    published = &words[(*cursor)++];

    uint32_t *fault = &words[*cursor];
    branch(below, fault, 3u); branch(overflow, fault, 2u); branch(above, fault, 8u);
    test(readable, fault, 0u); test(writable, fault, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17u, item->width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    uint32_t *fallback = &words[*cursor];
    if (unaligned != NULL) branch(unaligned, fallback, 1u);
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    emit_constant(words, cursor, 17u, HL_NATIVE_EXIT_FALLBACK);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(done, &words[*cursor], 14u);
    branch(published, &words[*cursor], 14u);
}

/* vector_lane 4 means the source is a single; the threshold is 2^31 or 2^63 in
 * that format, and the integer width picks x86's indefinite value. */
static void trunc_scalar_constants(const instruction *item, uint64_t *threshold, uint64_t *indefinite) {
    *threshold = item->vector_lane == 4u ?
                     (item->width == 8u ? UINT64_C(0x5f000000) : UINT64_C(0x4f000000)) :
                     (item->width == 8u ? UINT64_C(0x43e0000000000000) : UINT64_C(0x41e0000000000000));
    *indefinite = item->width == 8u ? UINT64_C(0x8000000000000000) : UINT64_C(0x80000000);
}

static uint32_t vector_operation_words(const instruction *item) {
    switch (item->vector_kind) {
    case VECTOR_BYTE_MASK: return 7u;
    case VECTOR_SCALAR_SQRT_DOUBLE:
    case VECTOR_SCALAR_ADD_DOUBLE:
    case VECTOR_SCALAR_MUL_DOUBLE:
    case VECTOR_SCALAR_SUB_DOUBLE:
    case VECTOR_SCALAR_DIV_DOUBLE:
        return 28u + constant_words(item->pc) + (item->live_chain != 0u ? 18u : 0u);
    case VECTOR_SCALAR_COMPARE_DOUBLE: return 13u;
    case VECTOR_TRUNC_DOUBLE_SIGNED: {
        uint64_t threshold;
        uint64_t indefinite;
        trunc_scalar_constants(item, &threshold, &indefinite);
        return 4u + constant_words(threshold) + constant_words(indefinite);
    }
    case VECTOR_FLOAT_ARITHMETIC:
        return 28u + constant_words(item->pc) + (item->live_chain != 0u ? 18u : 0u);
    case VECTOR_SHUFFLE_DWORD:
    case VECTOR_SHUFFLE_FLOAT:
        return 5u;
    case VECTOR_SHUFFLE_WORD: return 6u;
    case VECTOR_SHUFFLE_DOUBLE: return 3u;
    case VECTOR_BLEND_IMMEDIATE: {
        uint32_t count = 16u / item->vector_lane;
        uint32_t selected = 0u;
        uint32_t index;
        for (index = 0u; index < count; ++index)
            selected += (item->vector_immediate >> index) & 1u;
        return 2u + selected;
    }
    case VECTOR_BLEND_VARIABLE: return 3u;
    case VECTOR_MULTIPLY_EVEN_SIGNED_DWORD: return 3u;
    case VECTOR_HORIZONTAL_SUBTRACT:
    case VECTOR_HORIZONTAL_SATURATING: return 3u;
    case VECTOR_SIGN: return 6u;
    case VECTOR_ABSOLUTE: return 1u;
    case VECTOR_SIGNED_DWORD_TO_FLOAT: return 1u;
    case VECTOR_MERGE_FROM_INTEGER: return item->memory_operand != 0u ? 3u : 2u;
    case VECTOR_AES_ENCRYPT: return 5u;
    case VECTOR_AES_ENCRYPT_LAST: return 4u;
    case VECTOR_PACK_SIGNED:
    case VECTOR_PACK_UNSIGNED: return 3u;
    case VECTOR_FLOAT_TO_SIGNED_DWORD:
    case VECTOR_TRUNC_FLOAT_TO_SIGNED_DWORD:
        return 11u + constant_words(UINT64_C(0x4f000000)) +
               constant_words(UINT64_C(0x80000000));
    case VECTOR_STRING_EQUAL_EACH: return 96u;
    case VECTOR_INSERT_WORD: return 1u;
    case VECTOR_MULTIPLY_HIGH_WORD:
    case VECTOR_MULTIPLY_EVEN_DWORD: return 3u;
    case VECTOR_SUM_ABSOLUTE_DIFFERENCES_BYTE: return 4u;
    default: return 1u;
    }
}

uint32_t hl_x86_vector_words(const instruction *item) {
    uint32_t operation = vector_operation_words(item);
    if (item->vector_vex != 0u) {
        uint32_t upper_operation = vector_operation_words(item);
        uint32_t two_source = item->vector_kind == VECTOR_SHUFFLE_FLOAT ||
                              item->vector_kind == VECTOR_SHUFFLE_DOUBLE ||
                              item->vector_kind == VECTOR_ADD ||
                              item->vector_kind == VECTOR_SUBTRACT ||
                              item->vector_kind == VECTOR_MINIMUM_SIGNED ||
                              item->vector_kind == VECTOR_MINIMUM_UNSIGNED ||
                              item->vector_kind == VECTOR_MAXIMUM_SIGNED ||
                              item->vector_kind == VECTOR_MAXIMUM_UNSIGNED ||
                              item->vector_kind == VECTOR_UNPACK_LOW ||
                              item->vector_kind == VECTOR_UNPACK_HIGH ||
                              item->vector_kind == VECTOR_MULTIPLY_LOW_WORD ||
                              item->vector_kind == VECTOR_MULTIPLY_HIGH_WORD ||
                              item->vector_kind == VECTOR_MULTIPLY_EVEN_DWORD ||
                              item->vector_kind == VECTOR_MULTIPLY_EVEN_SIGNED_DWORD ||
                              item->vector_kind == VECTOR_MULTIPLY_LOW_DWORD ||
                              item->vector_kind == VECTOR_SIGN ||
                              item->vector_kind == VECTOR_PACK_SIGNED ||
                              item->vector_kind == VECTOR_PACK_UNSIGNED ||
                              item->vector_kind == VECTOR_AVERAGE_UNSIGNED ||
                              item->vector_kind == VECTOR_SUM_ABSOLUTE_DIFFERENCES_BYTE ||
                              item->vector_kind == VECTOR_HORIZONTAL_ADD ||
                              item->vector_kind == VECTOR_HORIZONTAL_SUBTRACT ||
                              item->vector_kind == VECTOR_HORIZONTAL_SATURATING;
        two_source = two_source || item->vector_kind == VECTOR_BLEND_IMMEDIATE ||
                     item->vector_kind == VECTOR_BLEND_VARIABLE;
        if (item->width == 32u && item->vector_kind == VECTOR_BLEND_IMMEDIATE &&
            item->condition == 0u) {
            instruction upper = *item;
            upper.vector_immediate = (uint8_t)(item->vector_immediate >> (16u / item->vector_lane));
            upper_operation = vector_operation_words(&upper);
        }
        operation += item->width == 32u ? upper_operation + 2u +
                         (item->memory_operand == 0u ? 2u : 0u) + (two_source != 0u ? 2u : 0u) : 2u;
        if (item->width == 32u && item->vector_kind == VECTOR_BLEND_VARIABLE) operation += 2u;
    }
    if (item->vector_memory_width == 32u && item->memory_operand != 0u &&
        (item->vector_kind == VECTOR_PACK_SIGNED || item->vector_kind == VECTOR_PACK_UNSIGNED))
        operation += 2u;
    if (item->memory_operand != 0u &&
        (item->vector_kind == VECTOR_HORIZONTAL_ADD ||
         item->vector_kind == VECTOR_HORIZONTAL_SUBTRACT ||
         item->vector_kind == VECTOR_HORIZONTAL_SATURATING)) {
        if (item->vector_memory_width == 16u)
            operation += item->vector_kind == VECTOR_HORIZONTAL_ADD ? 5u : 7u;
        else if (item->vector_kind != VECTOR_HORIZONTAL_ADD)
            operation += 2u;
    }
    uint32_t width = item->vector_memory_width != 0u ? item->vector_memory_width : item->width;
    uint32_t aligned = item->vector_aligned == 0u ? 0u :
                       6u + constant_words(item->pc) + (item->live_chain != 0u ? 19u : 0u);
    if (item->memory_operand == 0u) return operation;
    uint32_t words = hl_x86_address_words(item) - 1u +
           (item->memory_write != 0u ? 42u + HL_X86_WRITE_CACHE_WORDS : 24u) +
           (item->memory_write == 0u ? hl_x86_read_cache_words(width) : 0u) +
           (item->live_chain != 0u ? 19u : 0u) +
           constant_words(item->pc) +
           (item->vector_kind != VECTOR_COPY && item->memory_write == 0u ? operation : 0u) + aligned +
           (width == 32u ? (item->memory_write != 0u ? 3u : 4u) : 0u);
    if (item->vector_memory_width == 16u &&
        (item->vector_kind == VECTOR_UNPACK_LOW || item->vector_kind == VECTOR_UNPACK_HIGH))
        words += 5u;
    return words;
}

static void emit_vector_operation(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t destination = item->destination;
    uint32_t source = item->source;
    uint32_t first = item->vector_vex != 0u ? item->vector_source_one : destination;
    uint32_t scratch = item->vector_vex != 0u ? 20u : 17u;
    uint32_t lane = item->vector_lane;

    if (item->vector_kind == VECTOR_STRING_EQUAL_EACH) {
        uint64_t clear = UINT64_C(1) | UINT64_C(1) << 2 | UINT64_C(1) << 4 |
                         UINT64_C(1) << 6 | UINT64_C(1) << 7 | UINT64_C(1) << 11;
        uint32_t immediate = item->vector_immediate;

        /* IntRes1 for implicit-length byte equal-each.  Scratch vectors are
         * outside the guest XMM bank; operand two has already passed the
         * complete generic memory guard when it names v16. */
        words[(*cursor)++] = UINT32_C(0x6e208c00) | source << 16 | destination << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x4e209800) | destination << 5 | 19u;
        words[(*cursor)++] = UINT32_C(0x4e209800) | source << 5 | 21u;
#define EMIT_MASK(vector, target) do { \
            words[(*cursor)++] = UINT32_C(0x6f090400) | (vector) << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x6f001400) | 25u << 16 | 17u << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x6f001400) | 50u << 16 | 17u << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x6f001400) | 100u << 16 | 17u << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x0e003c00) | 1u << 16 | 17u << 5 | 16u; \
            words[(*cursor)++] = UINT32_C(0x0e003c00) | 17u << 16 | 17u << 5 | (target); \
            words[(*cursor)++] = UINT32_C(0x2a000000) | (target) << 16 | 8u << 10 | 16u << 5 | (target); \
        } while (0)
        EMIT_MASK(18u, 19u);
        EMIT_MASK(19u, 21u);
        EMIT_MASK(21u, 24u);
#undef EMIT_MASK
        emit_constant(words, cursor, 16u, UINT64_C(0x10000));
        words[(*cursor)++] = UINT32_C(0x2a000000) | 16u << 16 | 21u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac00000) | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac01000) | 17u << 5 | 22u;
        words[(*cursor)++] = UINT32_C(0x2a000000) | 16u << 16 | 24u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac00000) | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac01000) | 17u << 5 | 23u;
        emit_constant(words, cursor, 16u, 1u);
        words[(*cursor)++] = UINT32_C(0x1ac02000) | 22u << 16 | 16u << 5 | 25u;
        words[(*cursor)++] = UINT32_C(0x51000400) | 25u << 5 | 25u;
        /* w18, never w26: x26 carries the live instruction budget. */
        words[(*cursor)++] = UINT32_C(0x1ac02000) | 23u << 16 | 16u << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x51000400) | 18u << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x0a000000) | 25u << 16 | 19u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x0a000000) | 18u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x2a000000) | 18u << 16 | 25u << 5 | 16u;
        words[(*cursor)++] = UINT32_C(0x2a200000) | 16u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x53003c00) | 17u << 5 | 17u;
        if ((immediate & 0x10u) != 0u) {
            if ((immediate & 0x20u) != 0u) {
                words[(*cursor)++] = UINT32_C(0x4a000000) | 18u << 16 | 17u << 5 | 17u;
            } else {
                emit_constant(words, cursor, 16u, UINT64_C(0xffff));
                words[(*cursor)++] = UINT32_C(0x4a000000) | 16u << 16 | 17u << 5 | 17u;
            }
            words[(*cursor)++] = UINT32_C(0x53003c00) | 17u << 5 | 17u;
        }
        if ((immediate & 0x40u) == 0u) {
            emit_constant(words, cursor, 16u, UINT64_C(0x10000));
            words[(*cursor)++] = UINT32_C(0x2a000000) | 17u << 16 | 16u << 5 | 16u;
            words[(*cursor)++] = UINT32_C(0x5ac00000) | 16u << 5 | 16u;
            words[(*cursor)++] = UINT32_C(0x5ac01000) | 16u << 5 | 1u;
        } else {
            words[(*cursor)++] = UINT32_C(0x5ac01000) | 17u << 5 | 16u;
            emit_constant(words, cursor, 25u, 31u);
            words[(*cursor)++] = UINT32_C(0x4b000000) | 16u << 16 | 25u << 5 | 16u;
            words[(*cursor)++] = UINT32_C(0x7100001f) | 17u << 5;
            emit_constant(words, cursor, 25u, 16u);
            words[(*cursor)++] = UINT32_C(0x1a800000) | 16u << 16 | 0u << 12 | 25u << 5 | 1u;
        }

        /* Publish all six defined flags together after RCX is ready. */
        words[(*cursor)++] = load_word(20u, offsetof(hl_native_x86_64_cpu, flags));
        emit_constant(words, cursor, 25u, clear);
        words[(*cursor)++] = UINT32_C(0x8a200000) | 25u << 16 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x7100001f) | 17u << 5;
        words[(*cursor)++] = UINT32_C(0x1a9f07f9); /* cset w25,ne: CF */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x710042df); /* cmp w22,#16 */
        words[(*cursor)++] = UINT32_C(0x1a9f27f9); /* cset w25,lo */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 7u << 10 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x710042ff); /* cmp w23,#16 */
        words[(*cursor)++] = UINT32_C(0x1a9f27f9); /* cset w25,lo */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 6u << 10 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x12000239); /* and w25,w17,#1 */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 11u << 10 | 20u << 5 | 20u;
        words[(*cursor)++] = store_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    } else if (item->vector_kind >= VECTOR_SCALAR_SQRT_DOUBLE &&
        item->vector_kind <= VECTOR_SCALAR_DIV_DOUBLE) {
        uint32_t base = item->vector_kind == VECTOR_SCALAR_SQRT_DOUBLE ? UINT32_C(0x1e61c000) :
                        item->vector_kind == VECTOR_SCALAR_ADD_DOUBLE ? UINT32_C(0x1e602800) :
                        item->vector_kind == VECTOR_SCALAR_MUL_DOUBLE ? UINT32_C(0x1e600800) :
                        item->vector_kind == VECTOR_SCALAR_SUB_DOUBLE ? UINT32_C(0x1e603800) :
                                                                       UINT32_C(0x1e601800);
        if (item->vector_kind == VECTOR_SCALAR_SQRT_DOUBLE)
            words[(*cursor)++] = base | source << 5 | 18u;
        else
            words[(*cursor)++] = base | source << 16 | destination << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x1e602000) | 18u << 16 | 18u << 5; /* fcmp d18,d18 */
        uint32_t ordered = (*cursor)++;
        emit_constant(words, cursor, 16u, item->pc);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, program));
        emit_constant(words, cursor, 16u, HL_NATIVE_EXIT_FALLBACK);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, reason));
        if (item->live_chain != 0u) {
            hl_x86_finish_chain(words, cursor);
            hl_x86_spill_all(words, cursor);
        }
        words[(*cursor)++] = UINT32_C(0xd65f03c0);
        words[ordered] = UINT32_C(0x54000000) |
                         (((*cursor - ordered) & UINT32_C(0x7ffff)) << 5) | 7u; /* b.vc */
        words[(*cursor)++] = UINT32_C(0x6e080400) | 18u << 5 | destination; /* ins vd.d[0],v18.d[0] */
    } else if (item->vector_kind == VECTOR_FLOAT_ARITHMETIC) {
        uint32_t base;
        uint32_t ordered;
        if (item->condition != 0u) {
            base = item->vector_subopcode == 0x58u ? UINT32_C(0x1e202800) :
                   item->vector_subopcode == 0x59u ? UINT32_C(0x1e200800) :
                   item->vector_subopcode == 0x5cu ? UINT32_C(0x1e203800) : UINT32_C(0x1e201800);
            if (item->vector_lane == 8u) base |= UINT32_C(0x00400000);
            words[(*cursor)++] = base | source << 16 | destination << 5 | 18u;
            words[(*cursor)++] = (item->vector_lane == 8u ? UINT32_C(0x1e602000) :
                                                             UINT32_C(0x1e202000)) |
                                 18u << 16 | 18u << 5; /* fcmp result,result */
            ordered = (*cursor)++;
        } else {
            base = item->vector_subopcode == 0x58u ? UINT32_C(0x4e20d400) :
                   item->vector_subopcode == 0x59u ? UINT32_C(0x6e20dc00) :
                   item->vector_subopcode == 0x5cu ? UINT32_C(0x4ea0d400) : UINT32_C(0x6e20fc00);
            if (item->vector_lane == 8u) base |= UINT32_C(0x00400000);
            words[(*cursor)++] = base | source << 16 | destination << 5 | 18u;
            words[(*cursor)++] = (item->vector_lane == 8u ? UINT32_C(0x4e60e400) :
                                                             UINT32_C(0x4e20e400)) |
                                 18u << 16 | 18u << 5 | 19u; /* fcmeq result,result */
            words[(*cursor)++] = UINT32_C(0x6e31a800) | 19u << 5 | 19u; /* uminv b19,v19.16b */
            words[(*cursor)++] = UINT32_C(0x1e260000) | 19u << 5 | 16u; /* fmov w16,s19 */
            ordered = (*cursor)++;
        }
        emit_constant(words, cursor, 16u, item->pc);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, program));
        emit_constant(words, cursor, 16u, HL_NATIVE_EXIT_FALLBACK);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, reason));
        if (item->live_chain != 0u) {
            hl_x86_finish_chain(words, cursor);
            hl_x86_spill_all(words, cursor);
        }
        words[(*cursor)++] = UINT32_C(0xd65f03c0);
        words[ordered] = (item->condition != 0u ? UINT32_C(0x54000000) : UINT32_C(0x35000000)) |
                         (((*cursor - ordered) & UINT32_C(0x7ffff)) << 5) |
                         (item->condition != 0u ? 7u : 16u); /* b.vc / cbnz w16 */
        if (item->condition != 0u)
            words[(*cursor)++] = (item->vector_lane == 8u ? UINT32_C(0x6e080400) :
                                                             UINT32_C(0x6e040400)) |
                                 18u << 5 | destination;
        else
            words[(*cursor)++] = UINT32_C(0x4ea01c00) | 18u << 16 | 18u << 5 | destination;
    } else if (item->vector_kind == VECTOR_SCALAR_COMPARE_DOUBLE) {
        uint64_t clear = UINT64_C(1) | UINT64_C(1) << 2 | UINT64_C(1) << 4 |
                         UINT64_C(1) << 6 | UINT64_C(1) << 7 | UINT64_C(1) << 11;
        words[(*cursor)++] = (item->vector_lane == 4u ? UINT32_C(0x1e202000) : UINT32_C(0x1e602000)) |
                             (item->condition != 0u ? UINT32_C(0x10) : 0u) |
                             source << 16 | destination << 5; /* fcmp/fcmpe sd,ss or dd,ds */
        words[(*cursor)++] = UINT32_C(0x9a9f07e0) | (4u ^ 1u) << 12 | 19u; /* cset x19,mi: less */
        words[(*cursor)++] = UINT32_C(0x9a9f07e0) | (0u ^ 1u) << 12 | 20u; /* cset x20,eq */
        words[(*cursor)++] = UINT32_C(0x9a9f07e0) | (6u ^ 1u) << 12 | 21u; /* cset x21,vs: unordered */
        words[(*cursor)++] = load_word(22u, offsetof(hl_native_x86_64_cpu, flags));
        emit_constant(words, cursor, 23u, clear);
        words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 22u << 5 | 22u; /* bic flags,flags,mask */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 21u << 16 | 19u << 5 | 19u; /* less | unordered */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 19u << 16 | 22u << 5 | 22u; /* CF */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 21u << 16 | 20u << 5 | 20u; /* equal | unordered */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 20u << 16 | 6u << 10 | 22u << 5 | 22u; /* ZF */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 21u << 16 | 2u << 10 | 22u << 5 | 22u; /* PF */
        words[(*cursor)++] = store_word(22u, offsetof(hl_native_x86_64_cpu, flags));
    } else if (item->vector_kind == VECTOR_TRUNC_DOUBLE_SIGNED) {
        uint32_t single = item->vector_lane == 4u;
        uint64_t threshold;
        uint64_t indefinite;
        trunc_scalar_constants(item, &threshold, &indefinite);
        uint32_t convert = item->width == 8u ? UINT32_C(0x9e780000) : UINT32_C(0x1e780000);
        if (single != 0u) convert &= ~UINT32_C(0x00400000); /* type field 01 (double) -> 00 (single) */
        words[(*cursor)++] = convert | source << 5 | destination; /* fcvtzs x/w destination,s/d source */
        emit_constant(words, cursor, 20u, threshold);
        words[(*cursor)++] = (single != 0u ? UINT32_C(0x1e270000) : UINT32_C(0x9e670000)) |
                             20u << 5 | 19u; /* fmov s/d 19,w/x 20 */
        words[(*cursor)++] = (single != 0u ? UINT32_C(0x1e202000) : UINT32_C(0x1e602000)) |
                             19u << 16 | source << 5; /* fcmp source,19 */
        emit_constant(words, cursor, 20u, indefinite);
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9a800000) : UINT32_C(0x1a800000)) |
                             destination << 16 | 2u << 12 | 20u << 5 | destination; /* csel result,indef,result,cs */
    } else if (item->vector_kind == VECTOR_AES_ENCRYPT ||
               item->vector_kind == VECTOR_AES_ENCRYPT_LAST) {
        /* x86 adds the round key last, ARM's AESE adds it first, so AESE takes a
         * zero key and the real key is XORed in afterwards. */
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | destination << 16 |
                             destination << 5 | 18u; /* mov v18.16b,vd.16b */
        words[(*cursor)++] = UINT32_C(0x6e201c00) | 19u << 16 | 19u << 5 | 19u; /* eor v19,v19,v19 */
        words[(*cursor)++] = UINT32_C(0x4e284800) | 19u << 5 | 18u; /* aese v18.16b,v19.16b */
        if (item->vector_kind == VECTOR_AES_ENCRYPT)
            words[(*cursor)++] = UINT32_C(0x4e286800) | 18u << 5 | 18u; /* aesmc v18.16b,v18.16b */
        words[(*cursor)++] = UINT32_C(0x6e201c00) | source << 16 | 18u << 5 | destination;
    } else if (item->vector_kind == VECTOR_MERGE_FROM_INTEGER) {
        uint32_t wide = item->width == 8u;
        uint32_t single = item->vector_lane == 4u;
        uint32_t convert = UINT32_C(0x1e220000);
        uint32_t from = source;
        if (item->memory_operand != 0u) {
            /* The operand landed in v16; SCVTF reads a general register. */
            words[(*cursor)++] = (wide != 0u ? UINT32_C(0x9e660000) : UINT32_C(0x1e260000)) |
                                 16u << 5 | 20u; /* fmov w/x 20,s/d 16 */
            from = 20u;
        }
        if (wide != 0u) convert |= UINT32_C(0x80000000);
        if (single == 0u) convert |= UINT32_C(0x00400000);
        words[(*cursor)++] = convert | from << 5 | 19u; /* scvtf s/d 19,w/x from */
        words[(*cursor)++] = (single != 0u ? UINT32_C(0x6e040400) : UINT32_C(0x6e080400)) |
                             19u << 5 | destination; /* ins vd.s/d[0],v19.s/d[0] */
    } else if (item->vector_kind == VECTOR_MERGE_LOW) {
        words[(*cursor)++] = (item->width == 4u ? UINT32_C(0x6e040400) : UINT32_C(0x6e080400)) |
                             source << 5 | destination; /* ins vd.s/d[0],vn.s/d[0] */
    } else if (item->vector_kind == VECTOR_SIGNED_DWORD_TO_FLOAT) {
        words[(*cursor)++] = UINT32_C(0x4e21d800) | source << 5 | destination; /* scvtf .4s */
    } else if (item->vector_kind == VECTOR_FLOAT_TO_SIGNED_DWORD ||
               item->vector_kind == VECTOR_TRUNC_FLOAT_TO_SIGNED_DWORD) {
        /* Capture before writing so an in-place conversion still compares the float source. */
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | source << 16 | source << 5 | 17u;
        emit_constant(words, cursor, 16u, UINT64_C(0x4f000000));
        words[(*cursor)++] = UINT32_C(0x4e040c00) | 16u << 5 | 18u; /* dup 2^31 */
        emit_constant(words, cursor, 16u, UINT64_C(0x80000000));
        words[(*cursor)++] = UINT32_C(0x4e040c00) | 16u << 5 | 19u; /* dup indefinite */
        if (item->vector_kind == VECTOR_FLOAT_TO_SIGNED_DWORD) {
            words[(*cursor)++] = UINT32_C(0x6e219800) | 17u << 5 | destination; /* frintx */
            words[(*cursor)++] = UINT32_C(0x4ea1b800) | destination << 5 | destination;
        } else {
            words[(*cursor)++] = UINT32_C(0x4ea1b800) | 17u << 5 | destination; /* fcvtzs */
        }
        words[(*cursor)++] = UINT32_C(0x6e20e400) | 18u << 16 | 17u << 5 | 21u; /* >= 2^31 */
        words[(*cursor)++] = UINT32_C(0x4e20e400) | 17u << 16 | 17u << 5 | 22u; /* ordered */
        words[(*cursor)++] = UINT32_C(0x6e205800) | 22u << 5 | 22u; /* NaN mask */
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | 22u << 16 | 21u << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0x6e601c00) | destination << 16 | 19u << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | 21u << 16 | 21u << 5 | destination;
    } else if (item->vector_kind == VECTOR_SHUFFLE_DWORD) {
        unsigned output;
        for (output = 0; output < 4u; ++output) {
            unsigned input = (item->vector_immediate >> (2u * output)) & 3u;
            words[(*cursor)++] = UINT32_C(0x6e000400) | ((output << 3) | 4u) << 16 |
                                 (input << 2) << 11 | source << 5 | scratch;
        }
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_SHUFFLE_WORD) {
        unsigned output;
        unsigned high = item->condition != 0u ? 4u : 0u;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | source << 16 | source << 5 | scratch;
        for (output = 0; output < 4u; ++output) {
            unsigned input = high + ((item->vector_immediate >> (2u * output)) & 3u);
            words[(*cursor)++] = UINT32_C(0x6e000400) | (((high + output) << 2) | 2u) << 16 |
                                 (input << 1) << 11 | source << 5 | scratch;
        }
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_SHUFFLE_FLOAT) {
        unsigned output;
        for (output = 0; output < 4u; ++output) {
            unsigned input = (item->vector_immediate >> (2u * output)) & 3u;
            unsigned vector = output < 2u ? first : source;
            words[(*cursor)++] = UINT32_C(0x6e000400) | ((output << 3) | 4u) << 16 |
                                 (input << 2) << 11 | vector << 5 | scratch;
        }
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_SHUFFLE_DOUBLE) {
        unsigned low = item->vector_immediate & 1u;
        unsigned high = (item->vector_immediate >> 1) & 1u;
        words[(*cursor)++] = UINT32_C(0x6e000400) | ((0u << 4) | 8u) << 16 |
                             (low << 3) << 11 | first << 5 | scratch;
        words[(*cursor)++] = UINT32_C(0x6e000400) | ((1u << 4) | 8u) << 16 |
                             (high << 3) << 11 | source << 5 | scratch;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_BLEND_IMMEDIATE) {
        unsigned output;
        unsigned count = 16u / lane;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | first << 16 | first << 5 | scratch;
        for (output = 0; output < count; ++output) {
            if (((item->vector_immediate >> output) & 1u) != 0u) {
                unsigned imm5 = (output * lane * 2u) | lane;
                unsigned imm4 = output * lane;
                words[(*cursor)++] = UINT32_C(0x6e000400) | imm5 << 16 |
                                     imm4 << 11 | source << 5 | scratch;
            }
        }
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_BLEND_VARIABLE) {
        unsigned element_bits = lane * 8u;
        unsigned immhb = element_bits + 1u;
        words[(*cursor)++] = UINT32_C(0x4f000400) | immhb << 16 |
                             (uint32_t)item->vector_subopcode << 5 | scratch;
        words[(*cursor)++] = UINT32_C(0x6e601c00) | first << 16 | source << 5 | scratch;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_FROM_INTEGER)
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9e670000) : UINT32_C(0x1e270000)) |
                               source << 5 | destination;
    else if (item->vector_kind == VECTOR_TO_INTEGER)
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9e660000) : UINT32_C(0x1e260000)) |
                               source << 5 | destination;
    else if (item->vector_kind == VECTOR_ZERO_LOW)
        words[(*cursor)++] = UINT32_C(0x0ea01c00) | source << 16 | source << 5 | destination;
    else if (item->vector_kind == VECTOR_UNPACK_LOW || item->vector_kind == VECTOR_UNPACK_HIGH) {
        uint32_t base = lane == 1u ? UINT32_C(0x4e003800) : lane == 2u ? UINT32_C(0x4e403800) :
                        lane == 4u ? UINT32_C(0x4e803800) : UINT32_C(0x4ec03800);
        if (item->vector_kind == VECTOR_UNPACK_HIGH) base += UINT32_C(0x4000);
        words[(*cursor)++] = base | source << 16 | first << 5 | destination;
    } else if (item->vector_kind == VECTOR_INSERT_WORD) {
        uint32_t insert_lane = item->vector_immediate & 7u;
        words[(*cursor)++] = UINT32_C(0x4e001c00) | ((insert_lane << 2) | 2u) << 16 |
                             source << 5 | destination;
    } else if (item->vector_kind == VECTOR_COMPARE_EQUAL ||
               item->vector_kind == VECTOR_COMPARE_GREATER_SIGNED) {
        uint32_t base;
        if (item->vector_kind == VECTOR_COMPARE_EQUAL)
            base = lane == 1u ? UINT32_C(0x6e208c00) : lane == 2u ? UINT32_C(0x6e608c00) :
                   lane == 4u ? UINT32_C(0x6ea08c00) : UINT32_C(0x6ee08c00);
        else
            base = lane == 1u ? UINT32_C(0x4e203400) : lane == 2u ? UINT32_C(0x4e603400) :
                   lane == 4u ? UINT32_C(0x4ea03400) : UINT32_C(0x4ee03400);
        words[(*cursor)++] = base | source << 16 | first << 5 | destination;
    } else if (item->vector_kind == VECTOR_MINIMUM_SIGNED ||
               item->vector_kind == VECTOR_MINIMUM_UNSIGNED ||
               item->vector_kind == VECTOR_MAXIMUM_SIGNED ||
               item->vector_kind == VECTOR_MAXIMUM_UNSIGNED) {
        uint32_t is_unsigned = item->vector_kind == VECTOR_MINIMUM_UNSIGNED ||
                               item->vector_kind == VECTOR_MAXIMUM_UNSIGNED;
        uint32_t is_maximum = item->vector_kind == VECTOR_MAXIMUM_SIGNED ||
                              item->vector_kind == VECTOR_MAXIMUM_UNSIGNED;
        uint32_t base = (is_unsigned != 0u ? UINT32_C(0x6e206c00) : UINT32_C(0x4e206c00)) -
                        (is_maximum != 0u ? UINT32_C(0x800) : 0u);
        base |= lane == 1u ? 0u : lane == 2u ? UINT32_C(0x00400000) : UINT32_C(0x00800000);
        words[(*cursor)++] = base | source << 16 | first << 5 | destination;
    } else if (item->vector_kind == VECTOR_XOR) {
        words[(*cursor)++] = UINT32_C(0x6e201c00) | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_AND) {
        words[(*cursor)++] = UINT32_C(0x4e201c00) | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_OR) {
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_AND_NOT) {
        words[(*cursor)++] = UINT32_C(0x4e601c00) | destination << 16 | source << 5 | destination;
    } else if (item->vector_kind == VECTOR_ADD || item->vector_kind == VECTOR_SUBTRACT) {
        uint32_t base = item->vector_kind == VECTOR_ADD ? UINT32_C(0x4e208400) : UINT32_C(0x6e208400);
        base |= lane == 1u ? 0u : lane == 2u ? UINT32_C(0x00400000) :
                lane == 4u ? UINT32_C(0x00800000) : UINT32_C(0x00c00000);
        words[(*cursor)++] = base | source << 16 | first << 5 | destination;
    } else if (item->vector_kind == VECTOR_PACK_SIGNED ||
               item->vector_kind == VECTOR_PACK_UNSIGNED) {
        uint32_t size = lane == 4u ? UINT32_C(0x00400000) : 0u;
        uint32_t low = item->vector_kind == VECTOR_PACK_UNSIGNED ?
                           UINT32_C(0x2e212800) : UINT32_C(0x0e214800);
        uint32_t high = item->vector_kind == VECTOR_PACK_UNSIGNED ?
                            UINT32_C(0x6e212800) : UINT32_C(0x4e214800);
        words[(*cursor)++] = low | size | first << 5 | scratch;
        words[(*cursor)++] = high | size | source << 5 | scratch;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | scratch << 16 | scratch << 5 | destination;
    } else if (item->vector_kind == VECTOR_MULTIPLY_LOW_WORD) {
        words[(*cursor)++] = UINT32_C(0x4e609c00) | source << 16 |
                             first << 5 | destination; /* mul vd.8h,vfirst.8h,vs.8h */
    } else if (item->vector_kind == VECTOR_MULTIPLY_HIGH_WORD) {
        uint32_t low = item->condition != 0u ? UINT32_C(0x0e60c000) : UINT32_C(0x2e60c000);
        uint32_t high = item->condition != 0u ? UINT32_C(0x4e60c000) : UINT32_C(0x6e60c000);
        words[(*cursor)++] = low | source << 16 | first << 5 | 17u;
        words[(*cursor)++] = high | source << 16 | first << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x4e405800) | 18u << 16 | 17u << 5 | destination;
        /* UZP2 selects bits 31:16 from each widened product in lane order. */
    } else if (item->vector_kind == VECTOR_MULTIPLY_EVEN_DWORD) {
        words[(*cursor)++] = UINT32_C(0x4e801800) | first << 16 |
                             first << 5 | 17u; /* uzp1 even first-source lanes */
        words[(*cursor)++] = UINT32_C(0x4e801800) | source << 16 |
                             source << 5 | 18u; /* uzp1 even source lanes */
        words[(*cursor)++] = UINT32_C(0x2ea0c000) | 18u << 16 | 17u << 5 | destination;
    } else if (item->vector_kind == VECTOR_MULTIPLY_EVEN_SIGNED_DWORD) {
        words[(*cursor)++] = UINT32_C(0x4e801800) | first << 16 |
                             first << 5 | 17u; /* uzp1 even first-source lanes */
        words[(*cursor)++] = UINT32_C(0x4e801800) | source << 16 |
                             source << 5 | 18u; /* uzp1 even second-source lanes */
        words[(*cursor)++] = UINT32_C(0x0ea0c000) | 18u << 16 | 17u << 5 | destination;
    } else if (item->vector_kind == VECTOR_MULTIPLY_LOW_DWORD) {
        words[(*cursor)++] = UINT32_C(0x4ea09c00) | source << 16 |
                             first << 5 | destination; /* mul vd.4s,vn.4s,vm.4s */
    } else if (item->vector_kind == VECTOR_AVERAGE_UNSIGNED) {
        uint32_t base = lane == 1u ? UINT32_C(0x6e201400) : UINT32_C(0x6e601400);
        words[(*cursor)++] = base | source << 16 | first << 5 | destination;
    } else if (item->vector_kind == VECTOR_SUM_ABSOLUTE_DIFFERENCES_BYTE) {
        words[(*cursor)++] = UINT32_C(0x6e207400) | source << 16 | first << 5 | destination;
        words[(*cursor)++] = UINT32_C(0x6e202800) | destination << 5 | destination;
        words[(*cursor)++] = UINT32_C(0x6e602800) | destination << 5 | destination;
        words[(*cursor)++] = UINT32_C(0x6ea02800) | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_HORIZONTAL_ADD) {
        uint32_t base = lane == 2u ? UINT32_C(0x4e60bc00) : UINT32_C(0x4ea0bc00);
        words[(*cursor)++] = base | source << 16 | first << 5 | destination;
    } else if (item->vector_kind == VECTOR_HORIZONTAL_SUBTRACT ||
               item->vector_kind == VECTOR_HORIZONTAL_SATURATING) {
        uint32_t arrangement = lane == 2u ? UINT32_C(0x00400000) : UINT32_C(0x00800000);
        uint32_t base = item->vector_kind == VECTOR_HORIZONTAL_SUBTRACT ? UINT32_C(0x6e208400) :
                        item->condition != 0u ? UINT32_C(0x4e202c00) : UINT32_C(0x4e200c00);
        words[(*cursor)++] = UINT32_C(0x4e001800) | arrangement |
                             source << 16 | first << 5 | 17u; /* uzp1: adjacent even lanes */
        words[(*cursor)++] = UINT32_C(0x4e005800) | arrangement |
                             source << 16 | first << 5 | 18u; /* uzp2: adjacent odd lanes */
        words[(*cursor)++] = base | arrangement | 18u << 16 | 17u << 5 | destination;
    } else if (item->vector_kind == VECTOR_SIGN) {
        uint32_t size = lane == 1u ? 0u : lane == 2u ? UINT32_C(0x00400000) : UINT32_C(0x00800000);
        words[(*cursor)++] = UINT32_C(0x4e20a800) | size | source << 5 | 21u; /* cmlt control,#0 */
        words[(*cursor)++] = UINT32_C(0x4e208800) | size | source << 5 | 22u; /* cmgt control,#0 */
        words[(*cursor)++] = UINT32_C(0x6e20b800) | size | first << 5 | 23u;  /* neg first */
        words[(*cursor)++] = UINT32_C(0x4e201c00) | first << 16 | 22u << 5 | 22u;
        words[(*cursor)++] = UINT32_C(0x4e201c00) | 23u << 16 | 21u << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | 22u << 16 | 21u << 5 | destination;
    } else if (item->vector_kind == VECTOR_ABSOLUTE) {
        uint32_t size = lane == 1u ? 0u : lane == 2u ? UINT32_C(0x00400000) : UINT32_C(0x00800000);
        words[(*cursor)++] = UINT32_C(0x4e20b800) | size | source << 5 | destination;
    } else if (item->vector_kind == VECTOR_BYTE_MASK) {
        words[(*cursor)++] = UINT32_C(0x6f090400) | source << 5 | 17u; /* ushr v17.16b,source,#7 */
        words[(*cursor)++] = UINT32_C(0x6f001400) | 25u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x6f001400) | 50u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x6f001400) | 100u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x0e013e30); /* umov w16,v17.b[0] */
        words[(*cursor)++] = UINT32_C(0x0e113e20) | destination; /* umov wd,v17.b[8] */
        words[(*cursor)++] = UINT32_C(0x2a000000) | destination << 16 | 8u << 10 | 16u << 5 | destination;
    } else
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | source << 16 | source << 5 | destination;
}

static void emit_vex_completion(uint32_t *words, uint32_t *cursor, const instruction *item,
                                unsigned memory_upper) {
    instruction upper;
    if (item->vector_vex == 0u) return;
    if (item->width == 16u) {
        hl_x86_emit_vector_upper_zero(words, cursor, item->destination);
        return;
    }
    upper = *item; upper.width = 16u; upper.vector_memory_width = 0u;
    upper.memory_operand = 0u; upper.destination = 20u;
    if (memory_upper != 0u) upper.source = 17u;
    else { hl_x86_emit_vector_upper_load(words, cursor, 18u, item->source); upper.source = 18u; }
    if (item->vector_kind == VECTOR_SHUFFLE_FLOAT || item->vector_kind == VECTOR_SHUFFLE_DOUBLE ||
        item->vector_kind == VECTOR_ADD || item->vector_kind == VECTOR_SUBTRACT ||
        item->vector_kind == VECTOR_MULTIPLY_EVEN_SIGNED_DWORD ||
        item->vector_kind == VECTOR_MULTIPLY_LOW_DWORD ||
        item->vector_kind == VECTOR_SIGN ||
        item->vector_kind == VECTOR_PACK_SIGNED || item->vector_kind == VECTOR_PACK_UNSIGNED ||
        item->vector_kind == VECTOR_HORIZONTAL_ADD ||
        item->vector_kind == VECTOR_HORIZONTAL_SUBTRACT ||
        item->vector_kind == VECTOR_HORIZONTAL_SATURATING ||
        item->vector_kind == VECTOR_COMPARE_EQUAL ||
        item->vector_kind == VECTOR_COMPARE_GREATER_SIGNED ||
        item->vector_kind == VECTOR_MINIMUM_SIGNED || item->vector_kind == VECTOR_MINIMUM_UNSIGNED ||
        item->vector_kind == VECTOR_MAXIMUM_SIGNED || item->vector_kind == VECTOR_MAXIMUM_UNSIGNED ||
        item->vector_kind == VECTOR_UNPACK_LOW || item->vector_kind == VECTOR_UNPACK_HIGH ||
        item->vector_kind == VECTOR_MULTIPLY_LOW_WORD ||
        item->vector_kind == VECTOR_MULTIPLY_HIGH_WORD ||
        item->vector_kind == VECTOR_MULTIPLY_EVEN_DWORD ||
        item->vector_kind == VECTOR_AVERAGE_UNSIGNED ||
        item->vector_kind == VECTOR_SUM_ABSOLUTE_DIFFERENCES_BYTE ||
        item->vector_kind == VECTOR_BLEND_IMMEDIATE ||
        item->vector_kind == VECTOR_BLEND_VARIABLE) {
        hl_x86_emit_vector_upper_load(words, cursor, 19u, item->vector_source_one);
        upper.vector_source_one = 19u;
    } else upper.vector_source_one = upper.source;
    if (item->vector_kind == VECTOR_BLEND_VARIABLE) {
        hl_x86_emit_vector_upper_load(words, cursor, 21u, item->vector_subopcode);
        upper.vector_subopcode = 21u;
    } else if (item->vector_kind == VECTOR_BLEND_IMMEDIATE && item->condition == 0u) {
        upper.vector_immediate = (uint8_t)(item->vector_immediate >> (16u / item->vector_lane));
    }
    emit_vector_operation(words, cursor, &upper);
    hl_x86_emit_vector_upper_store(words, cursor, 20u, item->destination);
}

void hl_x86_emit_vector(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint32_t *skip;
    uint32_t *loaded_operation = NULL;
    uint32_t *cache_hit;
    unsigned required = item->memory_write != 0u ? 2u : 1u;
    uint32_t width = item->vector_memory_width != 0u ? item->vector_memory_width : item->width;
    int scalar_vector = item->vector_kind == VECTOR_INSERT_WORD;

    if (item->memory_operand == 0u) {
        emit_vector_operation(words, cursor, item);
        emit_vex_completion(words, cursor, item, 0u);
        return;
    }

    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    uint32_t *unaligned = NULL;
    if (item->vector_aligned != 0u) {
        words[(*cursor)++] = UINT32_C(0xf2400e1f); /* tst x16,#15 */
        unaligned = &words[(*cursor)++];
    }
    if (item->memory_write == 0u)
        hl_x86_emit_read_cache(words, cursor, width,
                               width == 32u ? 16u :
                                   item->vector_kind == VECTOR_COPY ? item->destination : 16u,
                               !scalar_vector, &cache_hit);
    else
        emit_write_cache(words, cursor, width);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211); /* add x17,x16,x17 */
    if (item->memory_write != 0u) {
        if (width == 32u) hl_x86_emit_vector_upper_load(words, cursor, 17u, item->source);
        words[(*cursor)++] = UINT32_C(0xd5033abf); /* dmb ishst: preserve x86 StoreStore ordering */
        words[(*cursor)++] = (width == 4u ? UINT32_C(0xbd000220) :
                               width == 8u ? UINT32_C(0xfd000220) : UINT32_C(0x3d800220)) |
                              item->source;
        if (width == 32u)
            words[(*cursor)++] = UINT32_C(0x3d800620) | 17u; /* str q17,[x17,#16] */
        words[(*cursor)++] = UINT32_C(0xd2800031);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
        hl_x86_emit_dirty(words, cursor);
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
        words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
        words[(*cursor)++] = UINT32_C(0xaa120231); /* orr x17,x17,x18: sticky permission latch */
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    } else {
        words[(*cursor)++] = (width == 2u ? UINT32_C(0x79400220) :
                               width == 4u ? UINT32_C(0xbd400220) :
                               width == 8u ? UINT32_C(0xfd400220) : UINT32_C(0x3dc00220)) |
                              (width == 32u ? 16u :
                                  item->vector_kind == VECTOR_COPY ? item->destination : 16u);
        if (width == 32u)
            words[(*cursor)++] = UINT32_C(0x3dc00620) | 17u; /* ldr q17,[x17,#16] */
        if (width == 32u && item->vector_kind == VECTOR_COPY) loaded_operation = &words[*cursor];
        words[(*cursor)++] = UINT32_C(0xd50339bf); /* dmb ishld: preserve x86 LoadLoad/LoadStore */
        if (width == 32u && item->vector_kind == VECTOR_COPY) {
            words[(*cursor)++] = UINT32_C(0x4ea01c00) | 16u << 16 | 16u << 5 | item->destination;
            hl_x86_emit_vector_upper_store(words, cursor, 17u, item->destination);
        } else if (item->vector_kind != VECTOR_COPY) {
            loaded_operation = &words[*cursor];
            emit_vector_operation(words, cursor, item);
            emit_vex_completion(words, cursor, item, width == 32u);
        }
    }
    skip = &words[(*cursor)++];

    {
        uint32_t *miss = &words[*cursor];
        branch(below, miss, 3u);
        branch(overflow, miss, 2u);
        branch(above, miss, 8u);
        test(permission, miss, required - 1u);
    }
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = required == 2u ? UINT32_C(0xd2800051) : UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17, width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    uint32_t *done = &words[*cursor];
    if (unaligned != NULL) {
        branch(unaligned, done, 1u);
        emit_constant(words, cursor, 17, item->pc);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
        words[(*cursor)++] = UINT32_C(0xd2800071);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
        if (item->live_chain != 0u) {
            hl_x86_finish_chain(words, cursor);
            hl_x86_spill_all(words, cursor);
        }
        words[(*cursor)++] = UINT32_C(0xd65f03c0);
    }
    branch(skip, &words[*cursor], 14u);
    if (item->memory_write == 0u)
        hl_x86_patch_read_hit(cache_hit,
                              item->vector_kind == VECTOR_COPY && width != 32u ?
                                  &words[*cursor] : loaded_operation);
}
