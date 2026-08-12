#include "../include/executor.h"
#include "../src/arena.h"
#include "support.h"

#include <stdio.h>
#include <string.h>

#define CHECK(condition)                                                                                               \
    do {                                                                                                               \
        if (!(condition)) {                                                                                            \
            fprintf(stderr, "lifecycle:%d: check failed: %s\n", __LINE__, #condition);                               \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

static int lifecycle(void) {
    test_memory memory_state = {0};
    hl_native_memory memory = test_services(&memory_state);
    hl_native_config request = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_diagnostics diagnostic = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostic)};
    CHECK(hl_native_create(&request, &executor) == HL_NATIVE_OK);
    CHECK(executor != NULL);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK);
    CHECK(diagnostic.capacity == request.capacity);
    CHECK(diagnostic.used == 0);
    CHECK(diagnostic.dual_alias == 0);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_STATE);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(memory_state.repair_calls == 1);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_STATE);
    hl_native_destroy(executor);
    CHECK(memory_state.release_calls == 1);
    return 0;
}

static int fallback(void) {
    test_memory memory_state = {.fail_dual = 1};
    hl_native_memory memory = test_services(&memory_state);
    hl_native_config request = test_config(&memory, HL_NATIVE_DUAL_PREFERRED);
    hl_native_executor *executor = NULL;
    CHECK(hl_native_create(&request, &executor) == HL_NATIVE_OK);
    CHECK(memory_state.reserve_calls == 2);
    hl_native_destroy(executor);

    memset(&memory_state, 0, sizeof(memory_state));
    memory_state.fail_dual = 1;
    executor = NULL;
    request = test_config(&memory, HL_NATIVE_DUAL_REQUIRED);
    CHECK(hl_native_create(&request, &executor) == HL_NATIVE_PLATFORM);
    CHECK(executor == NULL && memory_state.reserve_calls == 1);
    return 0;
}

static int publication(void) {
    test_memory memory_state = {0};
    hl_native_memory memory = test_services(&memory_state);
    hl_native_config request = test_config(&memory, HL_NATIVE_DUAL_REQUIRED);
    hl_native_arena arena;
    hl_native_span first, second;
    CHECK(hl_native_arena_create(&arena, &request) == HL_NATIVE_OK);
    CHECK(hl_native_arena_begin(&arena) == HL_NATIVE_OK);
    CHECK(hl_native_arena_allocate(&arena, 64, 16, &first) == HL_NATIVE_OK);
    CHECK(hl_native_arena_publish(&arena, &first, 32) == HL_NATIVE_OK);
    CHECK(hl_native_arena_publish(&arena, &first, 32) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_arena_allocate(&arena, request.capacity, 16, &second) == HL_NATIVE_CAPACITY);
    CHECK(hl_native_arena_allocate(&arena, 64, 64, &second) == HL_NATIVE_OK);
    CHECK(second.offset >= 32 && second.offset % 64 == 0);
    CHECK(hl_native_arena_publish(&arena, &second, 64) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&arena) == HL_NATIVE_OK);
    CHECK(memory_state.begin_calls == 1 && memory_state.end_calls == 1 && memory_state.publish_calls == 2);
    CHECK(arena.mapping.content == second.offset + 64);
    hl_native_arena_destroy(&arena);
    return 0;
}

static int validation(void) {
    test_memory memory_state = {0};
    hl_native_memory memory = test_services(&memory_state);
    hl_native_config request = test_config(&memory, 0);
    hl_native_executor *executor = (hl_native_executor *)(uintptr_t)1;
    request.capacity--;
    CHECK(hl_native_create(&request, &executor) == HL_NATIVE_ARGUMENT);
    CHECK(executor == NULL);
    CHECK(memory_state.reserve_calls == 0);
    return 0;
}

int main(void) {
    if (lifecycle() != 0 || fallback() != 0 || publication() != 0 || validation() != 0) return 1;
    return 0;
}
