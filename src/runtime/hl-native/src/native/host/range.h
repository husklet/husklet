#ifndef HL_HOST_RANGE_H
#define HL_HOST_RANGE_H

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

enum {
    HL_HOST_REGION_READ = 1u << 0,
    HL_HOST_REGION_WRITE = 1u << 1,
    HL_HOST_REGION_EXECUTE = 1u << 2,
};

typedef struct hl_host_region {
    uintptr_t address;
    size_t size;
    uint32_t protection;
} hl_host_region;

/* Native host VM page size, or zero when the host cannot provide a valid value. */
size_t hl_host_page_size(void);

/* True when the host page containing address is part of the process address space. */
int hl_host_address_mapped(uintptr_t address);

/* True when every host page touched by [address, address + size) is mapped. */
int hl_host_range_mapped(uintptr_t address, size_t size);

/* True when the aligned host page immediately below or above page_address is mapped. */
int hl_host_page_neighbor_mapped(uintptr_t page_address);

/* First mapped region containing address or beginning above it. Returns zero when none exists. */
int hl_host_region_query(uintptr_t address, hl_host_region *region);

/*
 * The subranges a mapping handle has given back.
 *
 * A mapping handle's addressing frame -- the base it was placed at and the length it was created
 * with -- is fixed for the life of the handle, because every offset-keyed operation on it (protect,
 * sync, unmap of a subrange) is relative to that base. A partial unmap therefore cannot move the
 * base or shrink the length. What it does change is which bytes of the frame the owner still has,
 * and that is a separate fact the registry has to carry: the ownership question -- may the address
 * space hand this address to someone else? -- has to be answered from what is still held, not from
 * the frame. Answering it from the frame leaves a handle claiming a hole it already gave back,
 * which refuses a genuinely vacant address and, worse, lets a whole-handle teardown unmap whatever
 * was placed in that hole in the meantime.
 *
 * Entries are kept sorted by offset, disjoint, and never adjacent: retiring a subrange merges it
 * with every entry it touches, so the count is the number of separate holes rather than the number
 * of calls. The set is empty -- entries NULL, no allocation ever made -- for every mapping that was
 * never partially unmapped, which is the overwhelming majority, so the ordinary path costs one
 * pointer test. These are inline because two of the three host registries that need them are built
 * without the rest of this translation unit.
 */
typedef struct hl_host_hole {
    uint64_t offset;
    uint64_t size;
} hl_host_hole;

typedef struct hl_host_hole_set {
    hl_host_hole *entries;
    uint32_t count;
    uint32_t capacity;
} hl_host_hole_set;

typedef int (*hl_host_range_unmap)(void *context, void *address, size_t size);

static inline void hl_host_hole_set_release(hl_host_hole_set *set) {
    if (set == NULL) return;
    free(set->entries);
    set->entries = NULL;
    set->count = 0;
    set->capacity = 0;
}

/* Record [offset, offset + size) as given back. Returns zero only when the set could not grow, and
 * then leaves it exactly as it was found: the caller goes on claiming that subrange, which refuses
 * a reuse that would have been legal but never hands away memory the owner still holds. */
static inline int hl_host_hole_set_retire(hl_host_hole_set *set, uint64_t offset, uint64_t size) {
    uint64_t low = offset;
    uint64_t high = offset + size;
    uint32_t write = 0;
    uint32_t index;
    uint32_t at;
    if (set == NULL || size == 0 || high < low) return 0;
    if (set->count == set->capacity) {
        uint32_t capacity = set->capacity == 0 ? 4u : set->capacity * 2u;
        hl_host_hole *grown;
        if (capacity <= set->capacity) return 0;
        grown = (hl_host_hole *)realloc(set->entries, (size_t)capacity * sizeof(*grown));
        if (grown == NULL) return 0;
        set->entries = grown;
        set->capacity = capacity;
    }
    /* Entries ascend and never touch each other, so one forward pass absorbs every entry the new
     * subrange reaches: [low, high) only ever grows, and it can only grow towards later entries. */
    for (index = 0; index < set->count; ++index) {
        uint64_t entry_low = set->entries[index].offset;
        uint64_t entry_high = entry_low + set->entries[index].size;
        if (entry_high >= low && entry_low <= high) {
            if (entry_low < low) low = entry_low;
            if (entry_high > high) high = entry_high;
        } else {
            set->entries[write++] = set->entries[index];
        }
    }
    /* Absorbing preserved the order of what is left, so one shift reseats the merged span. */
    at = write;
    while (at > 0 && set->entries[at - 1u].offset > low) {
        set->entries[at] = set->entries[at - 1u];
        --at;
    }
    set->entries[at].offset = low;
    set->entries[at].size = high - low;
    set->count = write + 1u;
    return 1;
}

/* True while any byte of [offset, offset + size) is still held. Entries are disjoint, so the bytes
 * they retire inside the query can simply be summed. */
static inline int hl_host_hole_set_holds(const hl_host_hole_set *set, uint64_t offset, uint64_t size) {
    uint64_t retired = 0;
    uint32_t index;
    if (size == 0) return 0;
    if (set == NULL || set->count == 0) return 1;
    for (index = 0; index < set->count; ++index) {
        uint64_t low = set->entries[index].offset;
        uint64_t high = low + set->entries[index].size;
        if (low < offset) low = offset;
        if (high > offset + size) high = offset + size;
        if (high > low) retired += high - low;
    }
    return retired < size;
}

/* The index-th still-held subrange of [0, span), ascending. Returns zero once index is past the
 * last one, so index zero returning zero means the handle holds nothing at all. */
static inline int hl_host_hole_set_held_range(const hl_host_hole_set *set, uint64_t span, uint32_t index,
                                              uint64_t *offset, uint64_t *size) {
    uint64_t cursor = 0;
    uint32_t seen = 0;
    uint32_t count = set == NULL ? 0u : set->count;
    uint32_t position;
    for (position = 0; position < count && cursor < span; ++position) {
        uint64_t low = set->entries[position].offset;
        uint64_t high = low + set->entries[position].size;
        if (low > span) low = span;
        if (low > cursor) {
            if (seen == index) {
                *offset = cursor;
                *size = low - cursor;
                return 1;
            }
            ++seen;
        }
        if (high > cursor) cursor = high;
    }
    if (cursor < span && seen == index) {
        *offset = cursor;
        *size = span - cursor;
        return 1;
    }
    return 0;
}

/* Record the successful release of one currently-held range without allocating. The released
 * range is necessarily bordered by an existing hole unless it is the whole frame; joining those
 * neighbors can only preserve or reduce the entry count. This is the bookkeeping primitive a
 * retryable whole-mapping teardown needs after the native VM has already accepted the unmap. */
static inline void hl_host_hole_set_release_held(hl_host_hole_set *set, uint64_t offset, uint64_t size) {
    uint64_t high = offset + size;
    uint32_t before = UINT32_MAX;
    uint32_t after = UINT32_MAX;
    uint32_t index;
    for (index = 0; index < set->count; ++index) {
        uint64_t hole_high = set->entries[index].offset + set->entries[index].size;
        if (hole_high == offset) before = index;
        if (set->entries[index].offset == high) after = index;
    }
    if (before != UINT32_MAX && after != UINT32_MAX) {
        set->entries[before].size = set->entries[after].offset + set->entries[after].size - set->entries[before].offset;
        for (index = after + 1u; index < set->count; ++index)
            set->entries[index - 1u] = set->entries[index];
        --set->count;
    } else if (before != UINT32_MAX) {
        set->entries[before].size += size;
    } else if (after != UINT32_MAX) {
        set->entries[after].offset = offset;
        set->entries[after].size += size;
    }
}

/* Release both aliases and every still-owned writable range while keeping failures retryable.
 * The executable alias goes first: if it fails, writable ownership is untouched. Every successful
 * writable unmap is recorded before the next native call, so a later failure leaves the handle
 * describing exactly the ranges that remain. */
static inline int hl_host_mapping_release(void **writable, void **executable, uint64_t span, hl_host_hole_set *retired,
                                          hl_host_range_unmap unmap, void *unmap_context) {
    uint64_t offset;
    uint64_t size;
    int dual = *executable != NULL && *executable != *writable;
    int single_alias = *executable != NULL && *executable == *writable;
    if (dual) {
        if (unmap(unmap_context, *executable, (size_t)span) != 0) return -1;
        *executable = NULL;
    }
    while (hl_host_hole_set_held_range(retired, span, 0, &offset, &size)) {
        if (unmap(unmap_context, (char *)*writable + offset, (size_t)size) != 0) return -1;
        if (retired->count == 0) {
            *writable = NULL;
            break;
        }
        hl_host_hole_set_release_held(retired, offset, size);
    }
    if (!hl_host_hole_set_holds(retired, 0, span)) *writable = NULL;
    if (single_alias) *executable = *writable;
    return 0;
}

#endif
