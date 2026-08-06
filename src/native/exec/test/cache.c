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

static hl_native_status publish_admitted(fixture *fixture, uint64_t guest, uint64_t epoch,
                                         uint32_t instruction_count, uint64_t admitted_offset) {
    hl_native_block block = {0};
    const hl_native_provenance provenance = {
        .code_size = 96, .guest = guest, .access = HL_NATIVE_ACCESS_UNKNOWN};
    hl_native_status status = hl_native_arena_begin(&fixture->arena);
    if (status != HL_NATIVE_OK) return status;
    status = hl_native_cache_reserve(fixture->cache, guest, epoch, guest, guest + 4, 96, &block);
    if (status == HL_NATIVE_OK) {
        uint32_t *words = (uint32_t *)(fixture->arena.writable + block.code_offset);
        for (uint32_t index = 0; index < 96 / sizeof(*words); index++)
            words[index] = UINT32_C(0xd503201f);
        block.instruction_count = instruction_count;
        block.admitted_offset = block.code_offset + admitted_offset;
        status = hl_native_cache_publish_map(fixture->cache, &block, 96, 4, &provenance, 1);
    }
    if (status != HL_NATIVE_OK && block.token != 0) hl_native_cache_cancel(fixture->cache, &block);
    if (hl_native_arena_end(&fixture->arena) != HL_NATIVE_OK && status == HL_NATIVE_OK)
        return HL_NATIVE_PLATFORM;
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
                                      0x7000, 0x7004, 16, NULL, &block) == HL_NATIVE_OK);
    memset(fixture.arena.writable + block.code_offset, 0xa5, 4);
    CHECK(hl_native_cache_publish(fixture.cache, &block, 4, 0) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&fixture.arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x7000, 5, 17, 0, 0, &code) == HL_NATIVE_HIT);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x7000, 5, 18, 0, 0, &code) == HL_NATIVE_MISS);
    fixture_destroy(&fixture);
    return 0;
}

static int probe_accounting(void) {
    fixture fixture;
    hl_native_code raw, legacy, counted, zero = {0}, sentinel;
    hl_native_cache_stats before, after;
    hl_native_cache_lookup_counts counts = {0};
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(publish(&fixture, 0x7100, 5, 0x7100, 0x7104) == HL_NATIVE_OK);
    hl_native_cache_diagnose(fixture.cache, &before);
    memset(&raw, 0xa5, sizeof(raw));
    CHECK(hl_native_cache_probe_key(fixture.cache, 0x7100, 5, 0, 0, 0, &raw) == HL_NATIVE_HIT);
    CHECK(hl_native_cache_probe_key(fixture.cache, 0x7200, 5, 0, 0, 0, &legacy) == HL_NATIVE_MISS);
    CHECK(memcmp(&legacy, &zero, sizeof(zero)) == 0);
    CHECK(hl_native_cache_probe_key(fixture.cache, 0x7100, 6, 0, 0, 0, &legacy) == HL_NATIVE_EPOCH);
    CHECK(memcmp(&legacy, &zero, sizeof(zero)) == 0);
    CHECK(hl_native_cache_probe_key(fixture.cache, 0x7100, 5, 1, 0, 0, &legacy) == HL_NATIVE_MISS);
    CHECK(memcmp(&legacy, &zero, sizeof(zero)) == 0);
    hl_native_cache_diagnose(fixture.cache, &after);
    CHECK(memcmp(&before, &after, sizeof(before)) == 0);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0x7100, 5, 0, 0, 0, &legacy) == HL_NATIVE_HIT);
    CHECK(memcmp(&raw, &legacy, sizeof(raw)) == 0);
    hl_native_cache_diagnose(fixture.cache, &before);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0x7100, 5, 0, 0, 0, &counted) == HL_NATIVE_HIT);
    CHECK(memcmp(&raw, &counted, sizeof(raw)) == 0);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0x7200, 5, 0, 0, 0, &counted) == HL_NATIVE_MISS);
    CHECK(memcmp(&counted, &zero, sizeof(zero)) == 0);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0x7100, 5, 1, 0, 0, &counted) == HL_NATIVE_MISS);
    CHECK(memcmp(&counted, &zero, sizeof(zero)) == 0);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0x7100, 6, 0, 0, 0, &counted) == HL_NATIVE_EPOCH);
    CHECK(memcmp(&counted, &zero, sizeof(zero)) == 0);
    hl_native_cache_diagnose(fixture.cache, &after);
    CHECK(memcmp(&before, &after, sizeof(before)) == 0);
    CHECK(counts.lookups == 4 && counts.hits == 1 && counts.misses == 2 && counts.epoch_rejections == 1);
    hl_native_cache_diagnose(fixture.cache, &before);
    CHECK(hl_native_cache_probe_key_counted(NULL, &counts, 0, 0, 0, 0, 0, &legacy) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, NULL, 0, 0, 0, 0, 0, &legacy) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0, 0, 0, 0, 0, NULL) == HL_NATIVE_MISS);
    hl_native_cache_diagnose(fixture.cache, &after);
    CHECK(memcmp(&before, &after, sizeof(before)) == 0 && counts.lookups == 4);
    counts = (hl_native_cache_lookup_counts){UINT64_MAX, UINT64_MAX, UINT64_MAX, UINT64_MAX};
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0x7100, 5, 0, 0, 0, &legacy) == HL_NATIVE_HIT);
    CHECK(counts.lookups == UINT64_MAX && counts.hits == UINT64_MAX &&
          counts.misses == UINT64_MAX && counts.epoch_rejections == UINT64_MAX);
    memset(&sentinel, 0xa5, sizeof(sentinel));
    before = after;
    CHECK(hl_native_cache_probe_key(NULL, 0, 0, 0, 0, 0, &sentinel) == HL_NATIVE_MISS);
    CHECK(((const uint8_t *)&sentinel)[0] == 0xa5);
    hl_native_cache_diagnose(fixture.cache, &after);
    CHECK(memcmp(&before, &after, sizeof(before)) == 0);
    hl_native_cache_fail(fixture.cache);
    hl_native_code poisoned_raw, poisoned_counted, poisoned_legacy;
    memset(&poisoned_raw, 0xa5, sizeof(poisoned_raw));
    poisoned_counted = poisoned_legacy = poisoned_raw;
    CHECK(hl_native_cache_probe_key(fixture.cache, 0, 0, 0, 0, 0, &poisoned_raw) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_probe_key_counted(fixture.cache, &counts, 0, 0, 0, 0, 0,
                                            &poisoned_counted) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_lookup_key(fixture.cache, 0, 0, 0, 0, 0, &poisoned_legacy) == HL_NATIVE_MISS);
    CHECK(memcmp(&poisoned_raw, &poisoned_counted, sizeof(poisoned_raw)) == 0 &&
          memcmp(&poisoned_raw, &poisoned_legacy, sizeof(poisoned_raw)) == 0);
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
                                      0x8000, 0x8004, 16, NULL, &block) == HL_NATIVE_OK);
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

static uint32_t *writable_entry(fixture *fixture, uint64_t guest) {
    hl_native_code code;
    if (hl_native_cache_lookup(fixture->cache, guest, 5, &code) != HL_NATIVE_HIT) return NULL;
    return (uint32_t *)(fixture->arena.writable +
                        ((const uint8_t *)code.entry - fixture->arena.executable));
}

static int relocation_span(void) {
    fixture fixture;
    uint32_t *source_words;
    const uint32_t cold = UINT32_C(0xa5a5a5a5);
    hl_native_relocation relocation = {
        .code_offset = 0,
        .target_guest = 0x9000,
        .span = {.word_count = 3, .cold = {cold, cold, cold}},
    };
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(publish(&fixture, 0x8000, 5, 0x8000, 0x8004) == HL_NATIVE_OK);
    source_words = writable_entry(&fixture, 0x8000);
    CHECK(source_words != NULL);

    /* A cold wildcard retains the entire reservation, then binds it when the
     * target appears without disturbing the unconsumed admission words. */
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_relocate(fixture.cache, 0x8000, 0, &relocation, 1) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    CHECK(source_words[0] == cold && source_words[1] == cold && source_words[2] == cold);
    CHECK(publish(&fixture, 0x9000, 5, 0x9000, 0x9004) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_resolve(fixture.cache, 0x9000, 0) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    CHECK(source_words[0] != cold && source_words[1] == cold && source_words[2] == cold);

    /* Target invalidation restores the full cold image before rebinding. */
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_relocations_invalidate(fixture.cache, 0x9000, 0x9004) == HL_NATIVE_OK);
    CHECK(hl_native_cache_invalidate(fixture.cache, 0x9000, 0x9004, NULL) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    CHECK(source_words[0] == cold && source_words[1] == cold && source_words[2] == cold);

    /* Reset retires both pending ownership and the source generation. */
    CHECK(hl_native_cache_reset(fixture.cache, 6) == HL_NATIVE_OK);
    CHECK(hl_native_cache_resolve(fixture.cache, 0x9000, 0) == HL_NATIVE_STATE);
    fixture_destroy(&fixture);

    /* The AArch64 typed span becomes one complete cross-entry transaction:
     * polls precede exact precharge, success enters after the target guard,
     * and budget rejection restores NZCV before the unchanged cold exit. */
    const uint32_t nop = UINT32_C(0xd503201f);
    hl_native_relocation admitted = {
        .code_offset = 0,
        .target_guest = 0xb000,
        .span = {.word_count = HL_NATIVE_RELOCATION_SPAN_WORDS,
                 .cold = {nop, nop, nop, nop, nop, nop, nop, nop,
                          nop, nop, nop, nop, nop, nop, nop, nop}},
    };
    CHECK(fixture_create(&fixture, 8, 5) == 0);
    CHECK(publish_admitted(&fixture, 0xa000, 5, 7, 0) == HL_NATIVE_OK);
    CHECK(publish_admitted(&fixture, 0xb000, 5, 23, 20) == HL_NATIVE_OK);
    source_words = writable_entry(&fixture, 0xa000);
    CHECK(source_words != NULL);
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_relocate(fixture.cache, 0xa000, 0, &admitted, 1) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    CHECK(source_words[0] != nop && source_words[1] == UINT32_C(0xb50001f0));
    CHECK(source_words[3] == UINT32_C(0xb4000070));
    CHECK(source_words[5] == UINT32_C(0xb5000170));
    CHECK(source_words[8] == (UINT32_C(0xf100021f) | (23u << 10)));
    CHECK(source_words[9] == UINT32_C(0x540000a3));
    CHECK(source_words[11] == (UINT32_C(0xd1000210) | (23u << 10)));
    CHECK(source_words[14] == UINT32_C(0xd51b4211));
    CHECK(source_words[15] == UINT32_C(0x14000001));
    CHECK(hl_native_cache_write_begin(fixture.cache) == HL_NATIVE_OK);
    CHECK(hl_native_cache_relocations_invalidate(fixture.cache, 0xb000, 0xb004) == HL_NATIVE_OK);
    CHECK(hl_native_cache_invalidate(fixture.cache, 0xb000, 0xb004, NULL) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(fixture.cache) == HL_NATIVE_OK);
    for (uint32_t index = 0; index < HL_NATIVE_RELOCATION_SPAN_WORDS; index++)
        CHECK(source_words[index] == nop);
    fixture_destroy(&fixture);
    return 0;
}

int main(void) {
    if (reuse() != 0 || invalidation() != 0 || epoch() != 0 || capacity() != 0 || isolation() != 0 ||
        instruction_epoch() != 0 || probe_accounting() != 0 || execution_identity() != 0 || authority_identity() != 0 ||
        relocation_observer() != 0 || relocation_span() != 0) return 1;
    return 0;
}
