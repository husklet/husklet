#ifndef HL_TRANSLATOR_X86_64_SMC_PAGE_INDEX_H
#define HL_TRANSLATOR_X86_64_SMC_PAGE_INDEX_H

#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

#define HL_SMC_PAGE_INDEX_LIVE UINT64_C(1)
#define HL_SMC_PAGE_INDEX_TOMB UINT64_C(2)

typedef struct hl_smc_page_index {
    _Atomic uint64_t *slots;
    size_t count;
} hl_smc_page_index;

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

static inline int hl_smc_page_index_add(hl_smc_page_index *index, uint64_t page) {
    uint64_t live = page | HL_SMC_PAGE_INDEX_LIVE;
    for (size_t attempt = 0; attempt < index->count; ++attempt) {
        size_t slot = (size_t)(hl_smc_page_index_hash(page) & (index->count - 1));
        size_t tomb = SIZE_MAX;
        for (size_t probe = 0; probe < index->count; ++probe) {
            uint64_t entry = atomic_load_explicit(&index->slots[slot], memory_order_acquire);
            if (entry == live) return 1;
            if ((entry & UINT64_C(0xfff)) == HL_SMC_PAGE_INDEX_TOMB && tomb == SIZE_MAX) tomb = slot;
            if (entry == 0) {
                size_t destination = tomb == SIZE_MAX ? slot : tomb;
                uint64_t expected = tomb == SIZE_MAX ? 0 : atomic_load_explicit(&index->slots[tomb], memory_order_acquire);
                if ((tomb == SIZE_MAX || (expected & UINT64_C(0xfff)) == HL_SMC_PAGE_INDEX_TOMB) &&
                    atomic_compare_exchange_strong_explicit(&index->slots[destination], &expected, live,
                                                            memory_order_release, memory_order_acquire))
                    return 1;
                break;
            }
            slot = (slot + 1) & (index->count - 1);
        }
    }
    return 0;
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

static inline void hl_smc_page_index_reset(hl_smc_page_index *index) {
    for (size_t slot = 0; slot < index->count; ++slot)
        atomic_store_explicit(&index->slots[slot], 0, memory_order_release);
}

#endif
