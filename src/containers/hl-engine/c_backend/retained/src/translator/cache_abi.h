#ifndef HL_TRANSLATOR_CACHE_ABI_H
#define HL_TRANSLATOR_CACHE_ABI_H

#include <stdint.h>

/*
 * Persistent caches contain executable host instructions. Their file format
 * and translator ABI are separate contracts: a layout-compatible file may
 * still contain code emitted under incompatible lowering or relocation rules.
 * Bump the matching ABI whenever those rules change.
 */
#define HL_PCACHE_ABI_AARCH64 UINT64_C(0x4136345043413032) /* "A64PCA02" */
#define HL_PCACHE_ABI_X86_64 UINT64_C(0x5838365043413031)  /* "X86PCA01" */

static inline int hl_pcache_compatible(uint64_t stored_format, uint64_t stored_abi, uint64_t current_format,
                                       uint64_t current_abi) {
    return stored_format == current_format && stored_abi == current_abi;
}

#endif
