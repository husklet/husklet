#ifndef HL_TRANSLATOR_X86_64_SMC_PAGE_INDEX_H
#define HL_TRANSLATOR_X86_64_SMC_PAGE_INDEX_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

#define HL_SMC_PAGE_INDEX_LIVE UINT64_C(1)
#define HL_SMC_PAGE_INDEX_TOMB UINT64_C(2)
#ifndef HL_SMC_PAGE_INDEX_BEFORE_CLAIM
#define HL_SMC_PAGE_INDEX_BEFORE_CLAIM() ((void)0)
#endif

#ifndef HL_SMC_PAGE_INDEX_LIFECYCLE_REMOVE
#define HL_SMC_PAGE_INDEX_LIFECYCLE_REMOVE(index, page) hl_smc_page_index_remove((index), (page))
#endif

typedef struct hl_smc_page_index {
    _Atomic uint64_t *slots;
    size_t count;
} hl_smc_page_index;

typedef enum hl_smc_page_index_add_result {
    HL_SMC_PAGE_INDEX_FULL = 0,
    HL_SMC_PAGE_INDEX_INSERTED = 1,
    HL_SMC_PAGE_INDEX_EXISTS = 2,
} hl_smc_page_index_add_result;

static inline uint64_t hl_smc_page_index_hash(uint64_t page) {
    uint64_t value = page >> 12;
    value ^= value >> 30;
    value *= UINT64_C(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value *= UINT64_C(0x94d049bb133111eb);
    return value ^ (value >> 31);
}

static inline int hl_smc_page_index_contains(const hl_smc_page_index *index, uint64_t page) {
    uint64_t live = page | HL_SMC_PAGE_INDEX_LIVE;
    size_t slot = (size_t)(hl_smc_page_index_hash(page) & (index->count - 1));
    for (size_t probe = 0; probe < index->count; ++probe) {
        uint64_t entry = atomic_load_explicit(&index->slots[slot], memory_order_acquire);
        if (entry == live) return 1;
        if (entry == 0) return 0;
        slot = (slot + 1) & (index->count - 1);
    }
    return 0;
}

static inline hl_smc_page_index_add_result hl_smc_page_index_add(hl_smc_page_index *index, uint64_t page) {
    uint64_t live = page | HL_SMC_PAGE_INDEX_LIVE;
    for (size_t attempt = 0; attempt < index->count; ++attempt) {
        size_t slot = (size_t)(hl_smc_page_index_hash(page) & (index->count - 1));
        size_t tomb = SIZE_MAX;
        for (size_t probe = 0; probe < index->count; ++probe) {
            uint64_t entry = atomic_load_explicit(&index->slots[slot], memory_order_acquire);
            if (entry == live) return HL_SMC_PAGE_INDEX_EXISTS;
            if ((entry & UINT64_C(0xfff)) == HL_SMC_PAGE_INDEX_TOMB && tomb == SIZE_MAX) tomb = slot;
            if (entry == 0) {
                size_t destination = tomb == SIZE_MAX ? slot : tomb;
                uint64_t expected = tomb == SIZE_MAX ? 0 : atomic_load_explicit(&index->slots[tomb], memory_order_acquire);
                HL_SMC_PAGE_INDEX_BEFORE_CLAIM();
                if ((tomb == SIZE_MAX || (expected & UINT64_C(0xfff)) == HL_SMC_PAGE_INDEX_TOMB) &&
                    atomic_compare_exchange_strong_explicit(&index->slots[destination], &expected, live,
                                                            memory_order_release, memory_order_acquire))
                    return HL_SMC_PAGE_INDEX_INSERTED;
                break;
            }
            slot = (slot + 1) & (index->count - 1);
        }
        if (tomb != SIZE_MAX) {
            uint64_t expected = atomic_load_explicit(&index->slots[tomb], memory_order_acquire);
            HL_SMC_PAGE_INDEX_BEFORE_CLAIM();
            if ((expected & UINT64_C(0xfff)) == HL_SMC_PAGE_INDEX_TOMB &&
                atomic_compare_exchange_strong_explicit(&index->slots[tomb], &expected, live, memory_order_release,
                                                        memory_order_acquire))
                return HL_SMC_PAGE_INDEX_INSERTED;
        }
    }
    return HL_SMC_PAGE_INDEX_FULL;
}

static inline int hl_smc_page_index_remove(hl_smc_page_index *index, uint64_t page) {
    uint64_t live = page | HL_SMC_PAGE_INDEX_LIVE;
    uint64_t tomb = page | HL_SMC_PAGE_INDEX_TOMB;
    size_t slot = (size_t)(hl_smc_page_index_hash(page) & (index->count - 1));
    for (size_t probe = 0; probe < index->count; ++probe) {
        uint64_t entry = atomic_load_explicit(&index->slots[slot], memory_order_acquire);
        if (entry == 0) return 0;
        if (entry == live)
            return atomic_compare_exchange_strong_explicit(&index->slots[slot], &entry, tomb, memory_order_acq_rel,
                                                           memory_order_acquire);
        slot = (slot + 1) & (index->count - 1);
    }
    return 0;
}

/* Called only while the mapping/JIT transaction owns all list writers.  The
   exact index remains atomic because translated and signal readers are lock
   free. */
static inline int hl_smc_page_registry_remove_range(hl_smc_page_index *index, uint64_t *pages, int *length,
                                                    uint64_t low, uint64_t high) {
    int removed = 0;
    for (int position = 0; position < *length;) {
        uint64_t page = pages[position];
        if (page < low || page >= high) {
            ++position;
            continue;
        }
        (void)HL_SMC_PAGE_INDEX_LIFECYCLE_REMOVE(index, page);
        pages[position] = pages[--*length];
        removed = 1;
    }
    return removed;
}

static inline void hl_smc_page_registry_remove_all(hl_smc_page_index *index, uint64_t *pages, int *length) {
    for (int position = 0; position < *length; ++position)
        (void)HL_SMC_PAGE_INDEX_LIFECYCLE_REMOVE(index, pages[position]);
    *length = 0;
}

#endif
