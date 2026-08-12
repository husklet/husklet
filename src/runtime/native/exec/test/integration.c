#include "support.h"

#include "../cache/cache.h"
#include "../src/arena.h"
#include "../src/fault/provenance.h"
#include "../src/executor.h"

#include <stdio.h>
#include <string.h>

#if defined(__APPLE__) && defined(__aarch64__)
#include "../src/arch/aarch64/entry.h"
#include <sys/ucontext.h>
#endif

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "line %d: %s\n", __LINE__, #expression);                                                   \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

static int cache_and_provenance(void) {
    test_memory host = {0};
    hl_native_memory services = test_services(&host);
    hl_native_config config = test_config(&services, 0);
    hl_native_arena arena;
    hl_native_cache *cache = NULL;
    hl_native_block block;
    hl_native_code code;
    hl_native_provenance records[2] = {
        {.code_offset = 0, .code_size = 4, .guest = 0x4000},
        {.code_offset = 4, .code_size = 8, .guest = 0x4004},
    };
    uint64_t guest = 0;

    CHECK(hl_native_arena_create(&arena, &config) == HL_NATIVE_OK);
    CHECK(hl_native_cache_create(&cache, &arena, 16, 16, 2, 7, NULL) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(cache, 0x4000, 7, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_arena_begin(&arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve(cache, 0x4000, 7, 0x4000, 0x4008, 32, &block) == HL_NATIVE_OK);
    memset(arena.writable + block.code_offset, 0xd5, 12);
    CHECK(hl_native_cache_publish_map(cache, &block, 12, 0, records, 2) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(cache, 0x4000, 7, &code) == HL_NATIVE_HIT);
    CHECK(hl_native_fault_guest(cache, (uint64_t)(uintptr_t)((unsigned char *)code.entry + 2), &guest));
    CHECK(guest == 0x4000);
    CHECK(hl_native_fault_guest(cache, (uint64_t)(uintptr_t)((unsigned char *)code.entry + 7), &guest));
    CHECK(guest == 0x4004);
    CHECK(hl_native_cache_lookup(cache, 0x4000, 8, &code) == HL_NATIVE_EPOCH);
    CHECK(host.begin_calls == 1 && host.end_calls == 1 && host.publish_calls == 1);

    hl_native_cache_destroy(cache);
    hl_native_arena_destroy(&arena);
    CHECK(host.release_calls == 1);
    return 0;
}

static int public_gates(void) {
    test_memory host = {0};
    hl_native_memory services = test_services(&host);
    hl_native_config config = test_config(&services, 0);
    hl_native_executor *executor = NULL;
    hl_native_change changes[2] = {
        {
            .abi = HL_NATIVE_ABI,
            .size = sizeof(changes[0]),
            .kind = HL_NATIVE_REPLACE,
            .mapping_epoch = 11,
        },
        {
            .abi = HL_NATIVE_ABI,
            .size = sizeof(changes[1]),
            .kind = HL_NATIVE_INVALIDATE,
            .reserved = 1,
            .first = 0x4000,
            .last = 0x5000,
        },
    };
    hl_native_diagnostics diagnostics = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostics)};
    hl_native_aarch64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                         .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_fault_scope scope = {0};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_changed(executor, changes, 2) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_diagnose(executor, &diagnostics) == HL_NATIVE_OK);
    CHECK(diagnostics.mapping_epoch == 0 && diagnostics.cache_generation == 1);
    CHECK(diagnostics.cache_lookups == 0 && diagnostics.cache_hits == 0 && diagnostics.cache_misses == 0);
    CHECK(hl_native_changed(executor, changes, 1) == HL_NATIVE_OK);
    CHECK(hl_native_diagnose(executor, &diagnostics) == HL_NATIVE_OK);
    CHECK(diagnostics.mapping_epoch == 11);
    CHECK(diagnostics.cache_generation == 2);

    hl_native_block block;
    hl_native_provenance source = {
        .code_offset = 0,
        .code_size = 4,
        .guest = UINT64_C(0x5100),
        .address = {.kind = HL_NATIVE_ADDRESS_BASE, .bits = 64, .base = 16},
        .access = HL_NATIVE_ACCESS_READ,
        .width = 8,
    };
    CHECK(hl_native_arena_begin(&executor->arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve(executor->cache, source.guest, 11, source.guest,
                                  source.guest + 4, 16, &block) == HL_NATIVE_OK);
    memset(executor->arena.writable + block.code_offset, 0xd5, 4);
    CHECK(hl_native_cache_publish_map(executor->cache, &block, 4, 0, &source, 1) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&executor->arena) == HL_NATIVE_OK);
    uint64_t host_pc = (uint64_t)(uintptr_t)(executor->arena.executable + block.code_offset);
    hl_native_provenance observed = {0};
    CHECK(hl_native_fault_scope_enter(executor, &cpu, &scope) == HL_NATIVE_OK);
    CHECK(hl_native_fault_scope_contains(&scope, host_pc));
    CHECK(hl_native_fault_scope_provenance(&scope, host_pc, &observed));
    CHECK(observed.guest == source.guest && observed.access == source.access &&
          observed.width == source.width);
#if defined(__APPLE__) && defined(__aarch64__)
    _STRUCT_MCONTEXT64 machine;
    ucontext_t context;
    memset(&machine, 0, sizeof(machine));
    memset(&context, 0, sizeof(context));
    context.uc_mcontext = &machine;
    state.host_stack = UINT64_C(0x1000);
    state.memory_delta = UINT64_C(0x1000);
    machine.__ss.__x[16] = UINT64_C(0x9000);
    machine.__ss.__x[28] = (uint64_t)(uintptr_t)&state;
    machine.__ss.__pc = host_pc + 4;
    CHECK(!hl_native_fault_scope_prepare_return(&scope, host_pc, UINT64_C(0x9004), &context));
    CHECK(state.reason == 0 && machine.__ss.__pc == host_pc + 4);
    machine.__ss.__pc = host_pc;
    CHECK(hl_native_fault_scope_prepare_return(&scope, host_pc, UINT64_C(0x9004), &context));
    CHECK(state.reason == HL_NATIVE_EXIT_FAULT && state.program == source.guest);
    CHECK(state.fault_address == UINT64_C(0x8004));
    CHECK(machine.__ss.__x[0] == (uint64_t)(uintptr_t)&state);
    CHECK(machine.__ss.__pc == (uint64_t)(uintptr_t)hl_native_aarch64_fault_return);
#endif
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_fault_scope_leave(&scope) == HL_NATIVE_OK);
    hl_native_destroy(executor);
    CHECK(host.release_calls == 1);
    return 0;
}

int main(void) {
    if (cache_and_provenance() != 0) return 1;
    if (public_gates() != 0) return 1;
    return 0;
}
