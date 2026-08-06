#include "translation.h"

#include <string.h>

static int certificate_eligible(const hl_native_certificate_record *record) {
    if (record->architecture != HL_NATIVE_AARCH64 && record->architecture != HL_NATIVE_X86_64)
        return 0;
    if (record->mapping_epoch == 0 || record->authority_identity == 0 ||
        record->source_last <= record->source_first)
        return 0;
    if (record->memory_mode == 0)
        return record->direct_token == 0 && record->direct_generation == 0;
    return record->direct_token != 0 && record->direct_generation != 0;
}

hl_native_lookup hl_native_translation_lookup(hl_native_executor *executor, const hl_native_translation_key *key,
                                              hl_native_code *output) {
    if (executor == NULL || key == NULL || output == NULL) return HL_NATIVE_MISS;
    return hl_native_cache_lookup_key(executor->cache, key->guest, key->mapping_incarnation,
                                      key->instruction_epoch, key->memory_mode,
                                      key->authority_generation, output);
}

hl_native_status hl_native_translation_publish(hl_native_executor *executor, const hl_native_translation_key *key,
                                               const hl_native_emission *emission) {
    hl_native_block block = {0};
    hl_native_status status;
    void *writable = NULL;
    uint64_t capacity = 0;
    int began = 0;
    int published = 0;
    int reserved = 0;
    hl_native_certificate_record certificate = {
        .architecture = key != NULL ? key->architecture : 0,
        .mapping_epoch = key != NULL ? key->mapping_incarnation : 0,
        .instruction_epoch = key != NULL ? key->instruction_epoch : 0,
        .memory_mode = key != NULL ? key->memory_mode : 0,
        .authority_identity = key != NULL ? key->authority_generation : 0,
        .direct_token = key != NULL ? key->direct_token : 0,
        .direct_generation = key != NULL ? key->direct_generation : 0,
        .source_first = key != NULL ? key->source_first : 0,
        .source_last = key != NULL ? key->source_last : 0,
    };
    if (executor == NULL || key == NULL || emission == NULL || emission->bytes == NULL ||
        emission->size == 0 || emission->body_offset >= emission->size ||
        emission->admitted_offset >= emission->size || emission->provenance == NULL ||
        emission->provenance_count == 0 || emission->provenance_count > HL_NATIVE_PROVENANCE_MAX ||
        (emission->relocation_count != 0 && emission->relocations == NULL) ||
        (emission->conditional_self_loop != 0 && emission->loop_pc != key->guest) ||
        key->source_last <= key->source_first || key->guest < key->source_first || key->guest >= key->source_last)
        return HL_NATIVE_ARGUMENT;
    status = hl_native_executor_gate_enter(executor);
    if (status != HL_NATIVE_OK) return status;
    status = hl_native_cache_write_begin(executor->cache);
    if (status == HL_NATIVE_OK) began = 1;
    if (status == HL_NATIVE_OK && certificate_eligible(&certificate))
        certificate.identity = hl_native_certificate_cache_identity_issue(executor);
    if (status == HL_NATIVE_OK)
        status = hl_native_cache_reserve_key(executor->cache, key->guest, key->mapping_incarnation,
                                             key->instruction_epoch, key->memory_mode,
                                             key->authority_generation, key->source_first, key->source_last,
                                             emission->size, &certificate, &block);
    if (status == HL_NATIVE_CAPACITY) {
        status = hl_native_executor_rollover(executor, key->mapping_incarnation, key->instruction_epoch);
        if (status == HL_NATIVE_OK)
            status = hl_native_cache_reserve_key(executor->cache, key->guest, key->mapping_incarnation,
                                                 key->instruction_epoch, key->memory_mode,
                                                 key->authority_generation, key->source_first, key->source_last,
                                                 emission->size, &certificate, &block);
    }
    if (status == HL_NATIVE_OK) reserved = 1;
    if (status == HL_NATIVE_OK) status = hl_native_cache_writable(executor->cache, &block, &writable, &capacity);
    if (status == HL_NATIVE_OK && capacity < emission->size) status = HL_NATIVE_CAPACITY;
    if (status == HL_NATIVE_OK) memcpy(writable, emission->bytes, emission->size);
    if (status == HL_NATIVE_OK) {
        block.instruction_count = emission->instruction_count != 0 ? emission->instruction_count : 1u;
        block.relocation_count = emission->relocation_count;
        block.admitted_offset = block.code_offset + emission->admitted_offset;
        block.conditional_self_loop = emission->conditional_self_loop;
        block.cycle_safe = emission->cycle_safe;
        block.loop_pc = emission->loop_pc;
        status = hl_native_cache_publish_map(executor->cache, &block, emission->size, emission->body_offset,
                                             emission->provenance, emission->provenance_count);
        if (status == HL_NATIVE_OK) {
            published = 1;
            reserved = 0;
        }
    }
    if (status == HL_NATIVE_OK && emission->relocation_count != 0)
        status = hl_native_cache_relocate(executor->cache, key->guest, key->instruction_epoch,
                                          emission->relocations, emission->relocation_count);
    if (status == HL_NATIVE_OK)
        status = hl_native_cache_resolve(executor->cache, key->guest, key->instruction_epoch);
    if (status != HL_NATIVE_OK && published) {
        hl_native_status relocation_cleanup = hl_native_cache_relocations_invalidate(
            executor->cache, key->source_first, key->source_last);
        hl_native_status cache_cleanup = hl_native_cache_invalidate(
            executor->cache, key->source_first, key->source_last, NULL);
        if (relocation_cleanup != HL_NATIVE_OK || cache_cleanup != HL_NATIVE_OK)
            hl_native_cache_fail(executor->cache);
    }
    if (reserved) hl_native_cache_cancel(executor->cache, &block);
    if (began) {
        hl_native_status end = hl_native_cache_write_end(executor->cache);
        if (status == HL_NATIVE_OK) status = end;
    }
    hl_native_executor_gate_leave(executor);
    return status;
}
