#include "provenance.h"

#include <stdint.h>

/* This path performs bounded reads only. It allocates no memory, takes no
 * lock, emits no diagnostic, and cannot unwind. Cache mutation is excluded by
 * dispatcher admission until provenance publication becomes atomic. */
int hl_native_fault_guest(const hl_native_cache *cache, uint64_t host_pc, uint64_t *guest_pc) {
    return hl_native_cache_provenance(cache, (const void *)(uintptr_t)host_pc, guest_pc);
}
