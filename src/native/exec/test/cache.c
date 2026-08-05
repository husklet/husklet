#include "../cache/cache.h"
#include "support.h"

#include <stdio.h>
#include <string.h>

#define CHECK(condition)                                                                                               \
    do {                                                                                                               \
        if (!(condition)) {                                                                                            \
            fprintf(stderr, "cache:%d: check failed: %s\n", __LINE__, #condition);                                   \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

typedef struct fixture {
    test_memory state;
    hl_native_memory memory;
    hl_native_config config;
    hl_native_arena arena;
    hl_native_cache *cache;
} fixture;

typedef struct observed_events {
    uint64_t counts[5];
} observed_events;

static void observe(void *context, hl_native_cache_event event) {
    observed_events *events = context;
    if (events != NULL && event >= HL_NATIVE_CACHE_RELOCATION_COLD_TARGET &&
        event <= HL_NATIVE_CACHE_RELOCATION_INVALIDATION)
        events->counts[event]++;
}

static int fixture_create_observed(fixture *fixture, uint32_t capacity, uint64_t epoch,
                                   const hl_native_cache_observer *observer) {
    memset(fixture, 0, sizeof(*fixture));
    fixture->memory = test_services(&fixture->state);
    fixture->config = test_config(&fixture->memory, HL_NATIVE_DUAL_REQUIRED);
    if (hl_native_arena_create(&fixture->arena, &fixture->config) != HL_NATIVE_OK) return 1;
    if (hl_native_cache_create(&fixture->cache, &fixture->arena, capacity, 8, 0, epoch, observer) != HL_NATIVE_OK) {
        hl_native_arena_destroy(&fixture->arena);
        return 1;
    }
    return 0;
}

static int fixture_create(fixture *fixture, uint32_t capacity, uint64_t epoch) {
    return fixture_create_observed(fixture, capacity, epoch, NULL);
}

static void fixture_destroy(fixture *fixture) {
    if (fixture->arena.writing) (void)hl_native_arena_end(&fixture->arena);
    hl_native_cache_destroy(fixture->cache);
    hl_native_arena_destroy(&fixture->arena);
}

static hl_native_status publish(fixture *fixture, uint64_t guest, uint64_t epoch, uint64_t first, uint64_t last) {
    hl_native_block block = {0};
    int reserved = 0;
    hl_native_status status = hl_native_arena_begin(&fixture->arena);
    if (status != HL_NATIVE_OK) return status;
    status = hl_native_cache_reserve(fixture->cache, guest, epoch, first, last, 64, &block);
    if (status == HL_NATIVE_OK) {
        reserved = 1;
        memset(fixture->arena.writable + block.code_offset, 0xa5, 32);
        status = hl_native_cache_publish(fixture->cache, &block, 32, 8);
    }
    if (status != HL_NATIVE_OK && reserved) hl_native_cache_cancel(fixture->cache, &block);
    if (hl_native_arena_end(&fixture->arena) != HL_NATIVE_OK && status == HL_NATIVE_OK) return HL_NATIVE_PLATFORM;
    return status;
}

static int reuse(void) {
    fixture fixture;
    hl_native_code code;
    hl_native_cache_stats stats;
    uint64_t guest;
    CHECK(fixture_create(&fixture, 8, 7) == 0);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x4000, 7, &code) == HL_NATIVE_MISS);
    CHECK(publish(&fixture, 0x4000, 7, 0x4000, 0x4010) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x4000, 7, &code) == HL_NATIVE_HIT);
    CHECK(code.entry == fixture.arena.executable && code.body == fixture.arena.executable + 8);
    CHECK(code.admitted == code.entry);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x4000, 7, &code) == HL_NATIVE_HIT);
    CHECK(hl_native_cache_provenance(fixture.cache, fixture.arena.executable + 12, &guest));
    CHECK(guest == 0x4000);
    hl_native_cache_diagnose(fixture.cache, &stats);
    CHECK(stats.misses == 1 && stats.hits == 2 && stats.publications == 1 && stats.live_blocks == 1);
    fixture_destroy(&fixture);
    return 0;
}

static int invalidation(void) {
    fixture fixture;
    hl_native_code code;
    uint32_t removed = 0;
    CHECK(fixture_create(&fixture, 8, 3) == 0);
    CHECK(publish(&fixture, 0x1000, 3, 0x1000, 0x1020) == HL_NATIVE_OK);
    CHECK(publish(&fixture, 0x2000, 3, 0x2000, 0x2020) == HL_NATIVE_OK);
    CHECK(hl_native_cache_invalidate(fixture.cache, 0x1010, 0x1030, &removed) == HL_NATIVE_OK);
    CHECK(removed == 1);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x1000, 3, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x2000, 3, &code) == HL_NATIVE_HIT);
    fixture_destroy(&fixture);
    return 0;
}

static int epoch(void) {
    fixture fixture;
    hl_native_code code;
    hl_native_cache_stats stats;
    CHECK(fixture_create(&fixture, 8, 11) == 0);
    CHECK(publish(&fixture, 0x3000, 11, 0x3000, 0x3010) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x3000, 12, &code) == HL_NATIVE_EPOCH);
    CHECK(hl_native_cache_reset(fixture.cache, 12) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x3000, 12, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x3000, 11, &code) == HL_NATIVE_EPOCH);
    hl_native_cache_diagnose(fixture.cache, &stats);
    CHECK(stats.epoch_rejections == 2 && stats.live_blocks == 0 && stats.mapping_epoch == 12);
    fixture_destroy(&fixture);
    return 0;
}

static int capacity(void) {
    fixture fixture;
    hl_native_block first, second;
    CHECK(fixture_create(&fixture, 2, 1) == 0);
    CHECK(hl_native_arena_begin(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve(fixture.cache, 1, 1, 1, 2, 64, &first) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve(fixture.cache, 2, 1, 2, 3, 64, &second) == HL_NATIVE_STATE);
    hl_native_cache_cancel(fixture.cache, &first);
    CHECK(hl_native_cache_invalidate(fixture.cache, 4, 4, NULL) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_cache_reserve(fixture.cache, 3, 1, 3, 4, fixture.config.capacity + 1, &second) ==
          HL_NATIVE_CAPACITY);
    CHECK(hl_native_arena_end(&fixture.arena) == HL_NATIVE_OK);
    CHECK(publish(&fixture, 1, 1, 1, 2) == HL_NATIVE_OK);
    CHECK(publish(&fixture, 2, 1, 2, 3) == HL_NATIVE_OK);
    CHECK(hl_native_arena_begin(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve(fixture.cache, 3, 1, 3, 4, 64, &second) == HL_NATIVE_CAPACITY);
    CHECK(hl_native_arena_end(&fixture.arena) == HL_NATIVE_OK);
    fixture_destroy(&fixture);
    return 0;
}

static int isolation(void) {
    fixture first, second;
    hl_native_code code;
    CHECK(fixture_create(&first, 8, 5) == 0);
    CHECK(fixture_create(&second, 8, 5) == 0);
    CHECK(publish(&first, 0x5000, 5, 0x5000, 0x5010) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(first.cache, 0x5000, 5, &code) == HL_NATIVE_HIT);
    CHECK(hl_native_cache_lookup(second.cache, 0x5000, 5, &code) == HL_NATIVE_MISS);
    fixture_destroy(&second);
    fixture_destroy(&first);
    return 0;
}

static int instruction_epoch(void) {
    fixture fixture;
    hl_native_block block = {0};
    hl_native_code code;
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(hl_native_arena_begin(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve_key(fixture.cache, 0x7000, 5, 17, 0, 0,
                                      0x7000, 0x7004, 16, &block) == HL_NATIVE_OK);
    memset(fixture.arena.writable + block.code_offset, 0xa5, 4);
    CHECK(hl_native_cache_publish(fixture.cache, &block, 4, 0) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x7000, 5, 17, 0, 0, &code) == HL_NATIVE_HIT);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x7000, 5, 18, 0, 0, &code) == HL_NATIVE_MISS);
    fixture_destroy(&fixture);
    return 0;
}

static int execution_identity(void) {
    fixture fixture;
    hl_native_code code;
    uint64_t identity;
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(publish(&fixture, 0x4000, 5, 0x4000, 0x4020) == HL_NATIVE_OK);
    CHECK(publish(&fixture, 0x4010, 5, 0x4010, 0x401c) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(fixture.cache, 0x4010, 5, &code) == HL_NATIVE_HIT);
    identity = ((uint64_t)(uintptr_t)code.body) | 1u;
    CHECK(hl_native_cache_execution(fixture.cache, identity, &code));
    CHECK(code.source_first == 0x4010 && code.source_last == 0x401c);
    CHECK(!hl_native_cache_execution(fixture.cache, identity & ~UINT64_C(1), &code));
    CHECK(!hl_native_cache_execution(fixture.cache, UINT64_MAX, &code));
    CHECK(hl_native_cache_reset(fixture.cache, 6) == HL_NATIVE_OK);
    CHECK(!hl_native_cache_execution(fixture.cache, identity, &code));
    fixture_destroy(&fixture);
    return 0;
}

static int authority_identity(void) {
    fixture fixture;
    hl_native_block block = {0};
    hl_native_code code;
    uint64_t stale_identity;
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(publish(&fixture, 0x8000, 5, 0x8000, 0x8004) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 0, 0, &code) == HL_NATIVE_HIT);
    stale_identity = ((uint64_t)(uintptr_t)code.body) | 1u;

    CHECK(hl_native_cache_reset_identity(fixture.cache, 5, 0, 1, 41) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 0, 0, &code) == HL_NATIVE_EPOCH);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 1, 41, &code) == HL_NATIVE_MISS);
    CHECK(!hl_native_cache_execution(fixture.cache, stale_identity, &code));

    CHECK(hl_native_arena_begin(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve_key(fixture.cache, 0x8000, 5, 0, 1, 41,
                                      0x8000, 0x8004, 16, &block) == HL_NATIVE_OK);
    memset(fixture.arena.writable + block.code_offset, 0xa5, 4);
    CHECK(hl_native_cache_publish(fixture.cache, &block, 4, 0) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 1, 41, &code) == HL_NATIVE_HIT);

    CHECK(hl_native_cache_reset_identity(fixture.cache, 5, 0, 1, 42) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 1, 41, &code) == HL_NATIVE_EPOCH);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 1, 42, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_reset_identity(fixture.cache, 5, 0, 0, 0) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x8000, 5, 0, 0, 0, &code) == HL_NATIVE_MISS);
    fixture_destroy(&fixture);
    return 0;
}

static int relocation_observer(void) {
    fixture fixture;
    observed_events events = {0};
    hl_native_cache_observer observer = {&events, observe};
    hl_native_relocation relocation = {
        .code_offset = 0,
        .target_guest = 0x9000,
        .target_epoch_known = 0,
        .expected = UINT32_C(0xa5a5a5a5),
    };
    CHECK(fixture_create_observed(&fixture, 8, 5, &observer) == 0);
    CHECK(publish(&fixture, 0x8000, 5, 0x8000, 0x8004) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_relocate(fixture.cache, 0x8000, 0, &relocation, 1) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    CHECK(events.counts[HL_NATIVE_CACHE_RELOCATION_COLD_TARGET] == 1);
    fixture_destroy(&fixture);
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(publish(&fixture, 0x8000, 5, 0x8000, 0x8004) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_relocate(fixture.cache, 0x8000, 0, &relocation, 1) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    CHECK(events.counts[HL_NATIVE_CACHE_RELOCATION_COLD_TARGET] == 1);
    fixture_destroy(&fixture);
    return 0;
}

int main(void) {
    if (reuse() != 0 || invalidation() != 0 || epoch() != 0 || capacity() != 0 || isolation() != 0 ||
        instruction_epoch() != 0 || execution_identity() != 0 || authority_identity() != 0 ||
        relocation_observer() != 0) return 1;
    return 0;
}
