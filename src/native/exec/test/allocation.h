#ifndef HL_NATIVE_TEST_ALLOCATION_H
#define HL_NATIVE_TEST_ALLOCATION_H

typedef __SIZE_TYPE__ hl_test_size;

void *hl_test_malloc(hl_test_size);
void *hl_test_calloc(hl_test_size, hl_test_size);
void *hl_test_aligned_alloc(hl_test_size, hl_test_size);
void *hl_test_msvc_aligned_alloc(hl_test_size, hl_test_size);
void hl_test_msvc_aligned_free(void *);
void hl_test_free(void *);

void hl_test_allocation_reset(hl_test_size);
hl_test_size hl_test_allocation_calls(void);
hl_test_size hl_test_allocation_live(void);

#ifndef HL_NATIVE_ALLOCATION_IMPLEMENTATION
#define malloc hl_test_malloc
#define calloc hl_test_calloc
#define aligned_alloc hl_test_aligned_alloc
#if defined(_MSC_VER)
#define _aligned_malloc hl_test_msvc_aligned_alloc
#define _aligned_free hl_test_msvc_aligned_free
#endif
#define free hl_test_free
#endif

#endif
