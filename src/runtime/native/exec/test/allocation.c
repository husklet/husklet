#define HL_NATIVE_ALLOCATION_IMPLEMENTATION
#include "allocation.h"

#undef malloc
#undef calloc
#undef aligned_alloc
#undef _aligned_malloc
#undef _aligned_free
#undef free

#include <stddef.h>
#include <stdlib.h>
#include <stdatomic.h>
#if defined(_MSC_VER)
#include <malloc.h>
#endif

static _Atomic size_t allocation_calls;
static _Atomic size_t allocation_live;
static _Atomic size_t allocation_failure;

static int allocation_admit(void) {
    size_t call = atomic_fetch_add_explicit(&allocation_calls, 1, memory_order_relaxed) + 1;
    return call != atomic_load_explicit(&allocation_failure, memory_order_relaxed);
}

void *hl_test_malloc(size_t size) {
    if (!allocation_admit()) return NULL;
    void *value = malloc(size);
    if (value != NULL) atomic_fetch_add_explicit(&allocation_live, 1, memory_order_relaxed);
    return value;
}

void *hl_test_calloc(size_t count, size_t size) {
    if (!allocation_admit()) return NULL;
    void *value = calloc(count, size);
    if (value != NULL) atomic_fetch_add_explicit(&allocation_live, 1, memory_order_relaxed);
    return value;
}

void *hl_test_aligned_alloc(size_t alignment, size_t size) {
    if (!allocation_admit()) return NULL;
#if defined(_MSC_VER)
    void *value = _aligned_malloc(size, alignment);
#else
    void *value = aligned_alloc(alignment, size);
#endif
    if (value != NULL) atomic_fetch_add_explicit(&allocation_live, 1, memory_order_relaxed);
    return value;
}

void *hl_test_msvc_aligned_alloc(size_t size, size_t alignment) {
    return hl_test_aligned_alloc(alignment, size);
}

void hl_test_msvc_aligned_free(void *value) {
    if (value != NULL) atomic_fetch_sub_explicit(&allocation_live, 1, memory_order_relaxed);
#if defined(_MSC_VER)
    _aligned_free(value);
#else
    free(value);
#endif
}

void hl_test_free(void *value) {
    if (value != NULL) atomic_fetch_sub_explicit(&allocation_live, 1, memory_order_relaxed);
    free(value);
}

void hl_test_allocation_reset(size_t failure) {
    atomic_store_explicit(&allocation_calls, 0, memory_order_relaxed);
    atomic_store_explicit(&allocation_failure, failure, memory_order_relaxed);
}

size_t hl_test_allocation_calls(void) {
    return atomic_load_explicit(&allocation_calls, memory_order_relaxed);
}

size_t hl_test_allocation_live(void) {
    return atomic_load_explicit(&allocation_live, memory_order_relaxed);
}
