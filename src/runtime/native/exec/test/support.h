#ifndef HL_NATIVE_TEST_SUPPORT_H
#define HL_NATIVE_TEST_SUPPORT_H

#include "../include/executor.h"

#include <stdlib.h>
#include <string.h>
#if defined(_MSC_VER)
#include <malloc.h>
#define test_aligned_allocate(alignment, size) _aligned_malloc((size), (alignment))
#define test_aligned_release(value) _aligned_free(value)
#else
#define test_aligned_allocate(alignment, size) aligned_alloc((alignment), (size))
#define test_aligned_release(value) free(value)
#endif

typedef struct test_memory {
    void *writable;
    void *executable;
    uint64_t capacity;
    uint64_t reserve_calls;
    uint64_t release_calls;
    uint64_t publish_calls;
    uint64_t begin_calls;
    uint64_t end_calls;
    uint64_t repair_calls;
    int fail_dual;
    int corrupt_mapping;
    int fail_repair;
    int fail_end;
} test_memory;

static hl_native_status test_reserve(void *opaque, uint64_t capacity, uint64_t alignment, uint32_t dual,
                                     hl_native_mapping *output) {
    test_memory *memory = opaque;
    memory->reserve_calls++;
    if (dual && memory->fail_dual) return HL_NATIVE_PLATFORM;
    memory->writable = test_aligned_allocate((size_t)alignment, (size_t)capacity);
    if (memory->writable == NULL) return HL_NATIVE_MEMORY;
    memory->executable = dual ? test_aligned_allocate((size_t)alignment, (size_t)capacity) : memory->writable;
    if (memory->executable == NULL) {
        test_aligned_release(memory->writable);
        memory->writable = NULL;
        return HL_NATIVE_MEMORY;
    }
    memory->capacity = capacity;
    memset(output, 0, sizeof(*output));
    output->abi = HL_NATIVE_ABI;
    output->size = sizeof(*output);
    output->handle = 1;
    output->writable = (uint64_t)(uintptr_t)memory->writable;
    output->executable = (uint64_t)(uintptr_t)memory->executable;
    output->capacity = capacity;
    if (memory->corrupt_mapping) output->capacity--;
    return HL_NATIVE_OK;
}

static hl_native_status test_release(void *opaque, hl_native_handle handle) {
    test_memory *memory = opaque;
    if (handle != 1) return HL_NATIVE_ARGUMENT;
    if (memory->executable != memory->writable) test_aligned_release(memory->executable);
    test_aligned_release(memory->writable);
    memory->writable = memory->executable = NULL;
    memory->release_calls++;
    return HL_NATIVE_OK;
}

static hl_native_status test_publish(void *opaque, hl_native_handle handle, uint64_t offset, uint64_t size) {
    test_memory *memory = opaque;
    if (handle != 1 || offset > memory->capacity || size > memory->capacity - offset) return HL_NATIVE_ARGUMENT;
    memory->publish_calls++;
    return HL_NATIVE_OK;
}

static hl_native_status test_repair(void *opaque, hl_native_mapping *mapping, uint32_t preserve) {
    test_memory *memory = opaque;
    (void)mapping;
    (void)preserve;
    memory->repair_calls++;
    return memory->fail_repair ? HL_NATIVE_PLATFORM : HL_NATIVE_OK;
}

static hl_native_status test_begin(void *opaque) {
    ((test_memory *)opaque)->begin_calls++;
    return HL_NATIVE_OK;
}

static hl_native_status test_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    if (memory->fail_end) return HL_NATIVE_PLATFORM;
    return HL_NATIVE_OK;
}

static hl_native_memory test_services(test_memory *memory) {
    hl_native_memory services = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(services),
        .context = memory,
        .reserve = test_reserve,
        .release = test_release,
        .publish = test_publish,
        .repair = test_repair,
        .write_begin = test_begin,
        .write_end = test_end,
    };
    return services;
}

static hl_native_config test_config(const hl_native_memory *memory, uint32_t flags) {
    hl_native_config config = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(config),
        .capacity = 64u << 20,
        .alignment = 4096,
        .flags = flags,
        .memory = memory,
    };
    return config;
}

#endif
