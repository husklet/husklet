#ifndef HL_TRANSLATOR_GUEST_X86_64_SMC_ADDRESS_H
#define HL_TRANSLATOR_GUEST_X86_64_SMC_ADDRESS_H

#include <stdint.h>

static inline int hl_smc_address_is_direct(int resolution) {
    return resolution == 0;
}

static inline uint64_t hl_smc_direct_page(uint64_t address, uint64_t nonpie_low, uint64_t nonpie_high,
                                          uint64_t nonpie_bias, uint64_t page_size) {
    if (nonpie_low != 0 && address >= nonpie_low && address < nonpie_high) address += nonpie_bias;
    return address & ~(page_size - 1);
}

#endif
