#include "storage.h"

#include <stddef.h>
#include <stdlib.h>
#if defined(_MSC_VER)
#include <malloc.h>
#endif

#define HL_NATIVE_IBTC_ALIGNMENT 65536u

_Static_assert(sizeof(hl_native_ibtc_entry) == 16, "IBTC entry footprint drifted");
_Static_assert(sizeof(hl_native_ibtc_authenticated_entry) == 32,
               "authenticated IBTC entry footprint drifted");
_Static_assert(_Alignof(hl_native_ibtc_authenticated_entry) == 32,
               "authenticated IBTC entry alignment drifted");
_Static_assert(offsetof(hl_native_ibtc_authenticated_entry, target) == 0,
               "authenticated IBTC target offset drifted");
_Static_assert(offsetof(hl_native_ibtc_authenticated_entry, authenticated_ingress) == 8,
               "authenticated IBTC ingress offset drifted");
_Static_assert(offsetof(hl_native_ibtc_authenticated_entry, target_identity) == 16,
               "authenticated IBTC identity offset drifted");
_Static_assert(offsetof(hl_native_ibtc_authenticated_entry, sequence) == 24,
               "authenticated IBTC sequence offset drifted");
_Static_assert(sizeof(((hl_native_ibtc_authenticated_entry *)0)->sequence) == sizeof(uint64_t),
               "authenticated IBTC atomic sequence width drifted");
_Static_assert(HL_NATIVE_IBTC_AUTHENTICATED_BYTES == 2u * 1024u * 1024u,
               "authenticated IBTC storage budget drifted");
_Static_assert(HL_NATIVE_IBTC_AUTHENTICATED_BYTES % HL_NATIVE_IBTC_ALIGNMENT == 0,
               "authenticated IBTC allocation multiple drifted");
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

int hl_native_ibtc_authenticated_storage_bytes(size_t count, size_t *output) {
    if (output == NULL || count > SIZE_MAX / sizeof(hl_native_ibtc_authenticated_entry))
        return 0;
    *output = count * sizeof(hl_native_ibtc_authenticated_entry);
    return 1;
}

hl_native_ibtc_authenticated_entry *hl_native_ibtc_authenticated_storage_create(void) {
    hl_native_ibtc_authenticated_entry *storage;
    size_t bytes;
    if (!hl_native_ibtc_authenticated_storage_bytes(HL_NATIVE_IBTC_COUNT, &bytes)) return NULL;
#if defined(_MSC_VER)
    storage = _aligned_malloc(bytes, HL_NATIVE_IBTC_ALIGNMENT);
#else
    storage = aligned_alloc(HL_NATIVE_IBTC_ALIGNMENT, bytes);
#endif
    if (storage != NULL) {
        for (size_t index = 0; index < HL_NATIVE_IBTC_COUNT; ++index) {
            storage[index].target = 0;
            storage[index].authenticated_ingress = 0;
            storage[index].target_identity = 0;
            atomic_init(&storage[index].sequence, 0);
        }
    }
    return storage;
}

void hl_native_ibtc_authenticated_storage_clear(hl_native_ibtc_authenticated_entry *storage) {
    if (storage == NULL) return;
    for (size_t index = 0; index < HL_NATIVE_IBTC_COUNT; ++index) {
        storage[index].target = 0;
        storage[index].authenticated_ingress = 0;
        storage[index].target_identity = 0;
        atomic_store_explicit(&storage[index].sequence, 0, memory_order_relaxed);
    }
}

void hl_native_ibtc_authenticated_storage_destroy(hl_native_ibtc_authenticated_entry *storage) {
#if defined(_MSC_VER)
    _aligned_free(storage);
#else
    free(storage);
#endif
}
