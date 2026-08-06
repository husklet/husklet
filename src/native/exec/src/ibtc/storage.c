#include "storage.h"

#include <stddef.h>
#include <stdlib.h>
#if defined(_MSC_VER)
#include <malloc.h>
#endif

#define HL_NATIVE_IBTC_ALIGNMENT 65536u

_Static_assert(sizeof(hl_native_ibtc_entry) == 16, "IBTC entry footprint drifted");
_Static_assert((HL_NATIVE_IBTC_COUNT * sizeof(hl_native_ibtc_entry)) % HL_NATIVE_IBTC_ALIGNMENT == 0,
               "IBTC storage must be an aligned-allocation multiple");

hl_native_ibtc_entry *hl_native_ibtc_storage_create(void) {
    const size_t bytes = HL_NATIVE_IBTC_COUNT * sizeof(hl_native_ibtc_entry);
#if defined(_MSC_VER)
    return _aligned_malloc(bytes, HL_NATIVE_IBTC_ALIGNMENT);
#else
    return aligned_alloc(HL_NATIVE_IBTC_ALIGNMENT, bytes);
#endif
}

void hl_native_ibtc_storage_destroy(hl_native_ibtc_entry *storage) {
#if defined(_MSC_VER)
    _aligned_free(storage);
#else
    free(storage);
#endif
}
