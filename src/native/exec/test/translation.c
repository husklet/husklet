#include "support.h"
#include "../src/translation.h"

#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "translation:%d: %s\n", __LINE__, #x); return 1; } } while (0)

typedef struct worker {
    hl_native_executor *executor;
    const hl_native_translation_key *key;
    const hl_native_emission *emission;
    _Atomic uint32_t *start;
    hl_native_status status;
} worker;

static void *publish(void *opaque) {
    worker *state = opaque;
    while (atomic_load_explicit(state->start, memory_order_acquire) == 0) { }
    state->status = hl_native_translation_publish(state->executor, state->key, state->emission);
    return NULL;
}

int main(void) {
    const hl_native_target_metadata absent = {0};
    const hl_native_target_metadata valid = {.certificate_literal_offset = 8,
                                             .authenticated_offset = 16};
    CHECK(hl_native_target_metadata_valid(&absent, 1));
    CHECK(hl_native_target_metadata_valid(&valid, 20));
    CHECK(!hl_native_target_metadata_valid(NULL, 20));
    CHECK(!hl_native_target_metadata_valid(&valid, 16));
    CHECK(!hl_native_target_metadata_valid(
        &(hl_native_target_metadata){.certificate_literal_offset = 9,
                                     .authenticated_offset = 17}, 32));
    CHECK(!hl_native_target_metadata_valid(
        &(hl_native_target_metadata){.certificate_literal_offset = 8,
                                     .authenticated_offset = 20}, 32));
    CHECK(!hl_native_target_metadata_valid(
        &(hl_native_target_metadata){.certificate_literal_offset = UINT64_MAX - 3,
                                     .authenticated_offset = 4}, UINT64_MAX));
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
                                .kind = HL_NATIVE_REPLACE, .mapping_epoch = 7};
    hl_native_change invalidate = {.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
                                   .kind = HL_NATIVE_INVALIDATE, .first = 0x4000, .last = 0x4004};
    const uint8_t bytes[] = {0x00, 0x00, 0x00, 0x14};
    const hl_native_provenance provenance = {.code_offset = 0, .code_size = 4, .guest = 0x4000};
    const hl_native_translation_key key = {0x4000, 7, 3, 0x4000, 0x4004, 0, 0, 0, 0, 0};
    const hl_native_emission emission = {.bytes = bytes, .size = sizeof(bytes), .body_offset = 0,
                                         .provenance = &provenance, .provenance_count = 1};
    hl_native_code code;
    pthread_t threads[2];
    _Atomic uint32_t start = 0;
    worker workers[2] = {{executor, &key, &emission, &start, HL_NATIVE_OK},
                         {executor, &key, &emission, &start, HL_NATIVE_OK}};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
#if defined(__aarch64__)
    if (getenv("HL_TRANSLATION_RELOCATION_ONLY") == NULL) {
    hl_native_execution held = {0};
    const uint32_t syscall_word = 0xd4000001u;
    const hl_native_source_span epoch_span = {
        0x6000, (const uint8_t *)&syscall_word, sizeof(syscall_word), 0, 2};
    const hl_native_source epoch_source = {&epoch_span, 1, 0, 2};
    hl_native_aarch64_cpu epoch_state = {.program = 0x6000};
    hl_native_cpu epoch_cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(epoch_cpu),
                               .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &epoch_state};
    hl_native_run_request epoch_request = {.abi = HL_NATIVE_ABI, .size = sizeof(epoch_request),
                                           .architecture = HL_NATIVE_AARCH64, .budget = 1,
                                           .source = &epoch_source};
    hl_native_exit epoch_exit = {.abi = HL_NATIVE_ABI, .size = sizeof(epoch_exit)};
    CHECK(hl_native_execution_enter(executor, &held) == HL_NATIVE_OK);
    CHECK(hl_native_run(executor, &epoch_cpu, &epoch_request, &epoch_exit) == HL_NATIVE_OK);
    CHECK(epoch_exit.kind == HL_NATIVE_EXIT_FALLBACK && epoch_exit.instruction == 0x6000);
    CHECK(hl_native_execution_leave(&held) == HL_NATIVE_OK);
    }
#endif
    workers[0].executor = executor;
    workers[1].executor = executor;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    CHECK(executor->authenticated_ibtc == NULL);
    CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_MISS);
    CHECK(pthread_create(&threads[0], NULL, publish, &workers[0]) == 0);
    CHECK(pthread_create(&threads[1], NULL, publish, &workers[1]) == 0);
    atomic_store_explicit(&start, 1, memory_order_release);
    CHECK(pthread_join(threads[0], NULL) == 0 && pthread_join(threads[1], NULL) == 0);
    CHECK((workers[0].status == HL_NATIVE_OK && workers[1].status == HL_NATIVE_STATE) ||
          (workers[1].status == HL_NATIVE_OK && workers[0].status == HL_NATIVE_STATE));
    CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_HIT);
    hl_native_translation_key stale = key;
    stale.instruction_epoch++;
    CHECK(hl_native_translation_lookup(executor, &stale, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_MISS);
    hl_native_emission excessive = emission;
    excessive.size = config.capacity + 1;
    stale.guest = stale.source_first = 0x5000;
    stale.source_last = 0x5004;
    CHECK(hl_native_translation_publish(executor, &stale, &excessive) == HL_NATIVE_CAPACITY);
    replace.mapping_epoch = 8;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &key, &code) == HL_NATIVE_EPOCH);

    const hl_native_translation_key target = {0x6000, 8, 5, 0x6000, 0x6004, 0, 0, 0, 0, 0};
    const hl_native_translation_key source = {0x7000, 8, 6, 0x7000, 0x7004, 0, 0, 0, 0, 0};
    const hl_native_provenance target_provenance = {.code_offset = 0, .code_size = 4, .guest = 0x6000};
    const hl_native_provenance source_provenance = {.code_offset = 0, .code_size = 4, .guest = 0x7000};
    const hl_native_relocation relocation = {.code_offset = 0, .target_guest = 0x6000,
                                             .target_instruction_epoch = 0, .target_epoch_known = 0,
                                             .expected = 0x14000000};
    const hl_native_emission target_emission = {.bytes = bytes, .size = sizeof(bytes), .body_offset = 0,
                                                .provenance = &target_provenance, .provenance_count = 1};
    const hl_native_emission source_emission = {.bytes = bytes, .size = sizeof(bytes), .body_offset = 0,
                                                .provenance = &source_provenance, .provenance_count = 1,
                                                .relocations = &relocation, .relocation_count = 1};
    CHECK(hl_native_translation_publish(executor, &target, &target_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &source, &source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &source, &code) == HL_NATIVE_HIT);
    const uint32_t *source_entry = code.entry;
    CHECK(*(const uint32_t *)code.entry != relocation.expected);
    invalidate = (hl_native_change){.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
        .kind = HL_NATIVE_INVALIDATE, .mapping_epoch = 8, .first = 0x6000, .last = 0x6004};
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*source_entry == relocation.expected);
    CHECK(hl_native_translation_lookup(executor, &source, &code) == HL_NATIVE_HIT);
    CHECK(hl_native_translation_lookup(executor, &target, &code) == HL_NATIVE_MISS);
    hl_native_translation_key target_new = target;
    target_new.instruction_epoch = 7;
    CHECK(hl_native_translation_publish(executor, &target_new, &target_emission) == HL_NATIVE_OK);
    CHECK(*source_entry != relocation.expected);
    invalidate.first = 0x7000;
    invalidate.last = 0x7004;
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*source_entry == relocation.expected);
    CHECK(hl_native_translation_lookup(executor, &source, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_translation_lookup(executor, &target_new, &code) == HL_NATIVE_HIT);

    CHECK(hl_native_translation_publish(executor, &source, &source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &source, &code) == HL_NATIVE_HIT);
    source_entry = code.entry;
    CHECK(*source_entry != relocation.expected);
    invalidate.first = 0x6000;
    invalidate.last = 0x6004;
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*source_entry == relocation.expected);
    CHECK(hl_native_translation_publish(executor, &target_new, &target_emission) == HL_NATIVE_OK);
    hl_native_translation_key failed_source = source;
    failed_source.guest = failed_source.source_first = 0x7500;
    failed_source.source_last = 0x7504;
    hl_native_provenance failed_provenance = source_provenance;
    failed_provenance.guest = 0x7500;
    hl_native_relocation failed_relocation = relocation;
    failed_relocation.expected = 0xdeadbeef;
    hl_native_emission failed_emission = source_emission;
    failed_emission.provenance = &failed_provenance;
    failed_emission.relocations = &failed_relocation;
    CHECK(hl_native_translation_publish(executor, &failed_source, &failed_emission) == HL_NATIVE_STATE);
    CHECK(hl_native_translation_lookup(executor, &failed_source, &code) == HL_NATIVE_MISS);

    replace.mapping_epoch = 9;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    hl_native_translation_key pending_source = source;
    pending_source.guest = pending_source.source_first = 0x8000;
    pending_source.source_last = 0x8004;
    pending_source.mapping_incarnation = 9;
    hl_native_translation_key pending_target = target;
    pending_target.guest = pending_target.source_first = 0x9000;
    pending_target.source_last = 0x9004;
    pending_target.mapping_incarnation = 9;
    pending_source.instruction_epoch = 11;
    pending_target.instruction_epoch = 12;
    hl_native_provenance pending_source_provenance = source_provenance;
    pending_source_provenance.guest = 0x8000;
    hl_native_provenance pending_target_provenance = target_provenance;
    pending_target_provenance.guest = 0x9000;
    hl_native_relocation pending_relocation = relocation;
    pending_relocation.target_guest = 0x9000;
    hl_native_emission pending_source_emission = source_emission;
    pending_source_emission.provenance = &pending_source_provenance;
    pending_source_emission.relocations = &pending_relocation;
    hl_native_emission pending_target_emission = target_emission;
    pending_target_emission.provenance = &pending_target_provenance;
    CHECK(hl_native_translation_publish(executor, &pending_source, &pending_source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &pending_source, &code) == HL_NATIVE_HIT);
    CHECK(*(const uint32_t *)code.entry == pending_relocation.expected);
    CHECK(hl_native_translation_publish(executor, &pending_target, &pending_target_emission) == HL_NATIVE_OK);
    CHECK(*(const uint32_t *)code.entry != pending_relocation.expected);

    replace.mapping_epoch = 10;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    pending_source.mapping_incarnation = 10;
    pending_target.mapping_incarnation = 10;
    CHECK(hl_native_translation_publish(executor, &pending_source, &pending_source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &pending_source, &code) == HL_NATIVE_HIT);
    CHECK(*(const uint32_t *)code.entry == pending_relocation.expected);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &pending_target, &pending_target_emission) == HL_NATIVE_OK);
    CHECK(*(const uint32_t *)code.entry == pending_relocation.expected);

    replace.mapping_epoch = 11;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    pending_source.mapping_incarnation = 11;
    pending_target.mapping_incarnation = 11;
    CHECK(hl_native_translation_publish(executor, &pending_target, &pending_target_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &pending_source, &pending_source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &pending_source, &code) == HL_NATIVE_HIT);
    source_entry = code.entry;
    CHECK(*source_entry != pending_relocation.expected);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(*source_entry == pending_relocation.expected);
    CHECK(hl_native_translation_lookup(executor, &pending_source, &code) == HL_NATIVE_HIT);
    replace.mapping_epoch = 10;
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

#if defined(__aarch64__)
    const uint32_t safe_source_words[] = {0x14000001u, 0xd65f03c0u};
    const uint32_t safe_target_words[] = {0xd65f03c0u};
    const hl_native_translation_key safe_source = {0xa000, 10, 7, 0xa000, 0xa008, 0, 0, 0, 0, 0};
    const hl_native_translation_key safe_target = {0xb000, 10, 7, 0xb000, 0xb004, 0, 0, 0, 0, 0};
    const hl_native_provenance safe_source_map = {.code_offset = 0, .code_size = 8, .guest = 0xa000};
    const hl_native_provenance safe_target_map = {.code_offset = 0, .code_size = 4, .guest = 0xb000};
    const hl_native_relocation safe_link = {.code_offset = 0, .target_guest = 0xb000,
        .target_instruction_epoch = 7, .target_epoch_known = 1, .expected = 0x14000001u};
    const hl_native_emission safe_source_emission = {.bytes = (const uint8_t *)safe_source_words,
        .size = sizeof(safe_source_words), .body_offset = 0, .provenance = &safe_source_map,
        .provenance_count = 1, .relocations = &safe_link, .relocation_count = 1};
    const hl_native_emission safe_target_emission = {.bytes = (const uint8_t *)safe_target_words,
        .size = sizeof(safe_target_words), .body_offset = 0, .provenance = &safe_target_map,
        .provenance_count = 1};
    CHECK(hl_native_translation_publish(executor, &safe_target, &safe_target_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_publish(executor, &safe_source, &safe_source_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &safe_source, &code) == HL_NATIVE_HIT);
    invalidate = (hl_native_change){.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
        .kind = HL_NATIVE_INVALIDATE, .mapping_epoch = 10, .first = 0xb000, .last = 0xb004};
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    CHECK(*(const uint32_t *)code.entry == safe_link.expected);

    const hl_native_translation_key cycle_left = {0xc000, 10, 9, 0xc000, 0xc004, 0, 0, 0, 0, 0};
    const hl_native_translation_key cycle_right = {0xd000, 10, 9, 0xd000, 0xd004, 0, 0, 0, 0, 0};
    const hl_native_provenance cycle_left_map = {.code_offset = 0, .code_size = 4, .guest = 0xc000};
    const hl_native_provenance cycle_right_map = {.code_offset = 0, .code_size = 4, .guest = 0xd000};
    const hl_native_relocation cycle_left_link = {.code_offset = 0, .target_guest = 0xd000,
        .target_instruction_epoch = 9, .target_epoch_known = 1, .expected = 0x14000000u};
    const hl_native_relocation cycle_right_link = {.code_offset = 0, .target_guest = 0xc000,
        .target_instruction_epoch = 9, .target_epoch_known = 1, .expected = 0x14000000u};
    const hl_native_emission cycle_left_emission = {.bytes = bytes, .size = sizeof(bytes), .body_offset = 0,
        .provenance = &cycle_left_map, .provenance_count = 1,
        .relocations = &cycle_left_link, .relocation_count = 1};
    const hl_native_emission cycle_right_emission = {.bytes = bytes, .size = sizeof(bytes), .body_offset = 0,
        .provenance = &cycle_right_map, .provenance_count = 1,
        .relocations = &cycle_right_link, .relocation_count = 1};
    CHECK(hl_native_translation_publish(executor, &cycle_left, &cycle_left_emission) == HL_NATIVE_OK);
    CHECK(hl_native_translation_lookup(executor, &cycle_left, &code) == HL_NATIVE_HIT);
    const uint32_t *cycle_left_entry = code.entry;
    CHECK(*cycle_left_entry == cycle_left_link.expected);
    CHECK(hl_native_translation_publish(executor, &cycle_right, &cycle_right_emission) == HL_NATIVE_OK);
    CHECK(*cycle_left_entry == cycle_left_link.expected);
    CHECK(hl_native_translation_lookup(executor, &cycle_right, &code) == HL_NATIVE_HIT);
    CHECK(*(const uint32_t *)code.entry != cycle_right_link.expected);
#endif
    hl_native_destroy(executor);
    CHECK(host.release_calls == 1);
    return 0;
}
