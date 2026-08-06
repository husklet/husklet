#include "private.h"

#include "../../cpu/include/layout.h"

#include <stddef.h>
#include <string.h>

/* The imported cache closed every resolved cycle. Restrict that existing
 * behavior: an unguarded cycle retains one typed dispatcher exit. Publication
 * cannot allocate, so admission uses a fixed nonrecursive frontier. A guarded
 * cycle may close only when every examined entry has its own interrupt and
 * budget checkpoint. Saturation conservatively retains the original exit. */
enum { CYCLE_FRONTIER_CAPACITY = 64 };

typedef struct cycle_node { uint64_t guest, epoch; } cycle_node;

static uint32_t span_words(const hl_native_relocation *relocation) {
    return relocation->span.word_count == 0 ? 1 : relocation->span.word_count;
}

static uint32_t cold_word(const hl_native_relocation *relocation, uint32_t index) {
    return relocation->span.word_count == 0 ? relocation->expected : relocation->span.cold[index];
}

static int span_matches(const uint32_t *words, const hl_native_relocation *relocation,
                        uint32_t first) {
    uint32_t count = span_words(relocation);
    for (uint32_t index = 0; index < count; index++) {
        uint32_t expected = index == 0 ? first : cold_word(relocation, index);
        if (words[index] != expected) return 0;
    }
    return 1;
}

static int hot_span_matches(const uint32_t *words, const resolved_relocation *resolved) {
    uint32_t count = span_words(&resolved->relocation);
    for (uint32_t index = 0; index < count; index++)
        if (words[index] != resolved->patched[index]) return 0;
    return 1;
}

static uint32_t a64_imm19(uint32_t instruction, int64_t displacement) {
    return instruction | (((uint32_t)displacement & UINT32_C(0x7ffff)) << 5);
}

static uint32_t a64_branch(uint64_t source, uint64_t target) {
    int64_t displacement = target >= source
        ? (int64_t)((target - source) / 4)
        : -(int64_t)((source - target) / 4);
    return UINT32_C(0x14000000) | ((uint32_t)displacement & UINT32_C(0x03ffffff));
}

static uint32_t a64_imm14(uint32_t instruction, int64_t displacement) {
    return instruction | (((uint32_t)displacement & UINT32_C(0x3fff)) << 5);
}

/* Budgets never exceed HL_NATIVE_MAX_BUDGET, so testing the sign of
 * `budget - count` is exactly the unsigned `budget < count` the retained engine
 * spells with `cmp`, and it leaves the guest NZCV live in the host flags. */
static int a64_edge_admission(uint32_t *hot, uint64_t source_offset,
                              const cache_entry *target) {
    const uint32_t count = target->instruction_count;
    const uint64_t branch_offset = source_offset + 10u * 4u;
    int64_t displacement = target->admitted_offset >= branch_offset
        ? (int64_t)((target->admitted_offset - branch_offset) / 4)
        : -(int64_t)((branch_offset - target->admitted_offset) / 4);
    if (count == 0 || count > UINT32_C(0xfff) ||
        ((target->admitted_offset | branch_offset) & 3u) != 0 ||
        displacement < -(1 << 25) || displacement >= (1 << 25))
        return 0;
    hot[0] = UINT32_C(0xf9400000) |
        ((uint32_t)(offsetof(hl_native_aarch64_cpu, interrupt) / 8) << 10) | (28u << 5) | 16u;
    hot[1] = a64_imm19(UINT32_C(0xb5000010), 15); /* cbnz x16,cold */
    hot[2] = UINT32_C(0xf9400000) |
        ((uint32_t)(offsetof(hl_native_aarch64_cpu, interrupt_token) / 8) << 10) | (28u << 5) | 16u;
    hot[3] = a64_imm19(UINT32_C(0xb4000010), 3); /* cbz x16,budget */
    hot[4] = UINT32_C(0xc8dffe10);              /* ldar x16,[x16] */
    hot[5] = a64_imm19(UINT32_C(0xb5000010), 11); /* cbnz x16,cold */
    hot[6] = UINT32_C(0xf9400000) |
        ((uint32_t)(offsetof(hl_native_aarch64_cpu, budget) / 8) << 10) | (28u << 5) | 16u;
    hot[7] = UINT32_C(0xd1000210) | (count << 10); /* sub x16,x16,#count */
    hot[8] = a64_imm14(UINT32_C(0xb7f80010), 7); /* tbnz x16,#63,cold */
    hot[9] = UINT32_C(0xf9000000) |
        ((uint32_t)(offsetof(hl_native_aarch64_cpu, budget) / 8) << 10) | (28u << 5) | 16u;
    hot[10] = a64_branch(branch_offset, target->admitted_offset);
    hot[11] = UINT32_C(0xd503201f);
    hot[12] = UINT32_C(0xd503201f);
    hot[13] = UINT32_C(0xd503201f);
    hot[14] = UINT32_C(0xd503201f);
    hot[15] = UINT32_C(0x14000001); /* b cold */
    return 1;
}

static void span_restore(uint32_t *words, const hl_native_relocation *relocation) {
    uint32_t count = span_words(relocation);
    for (uint32_t index = 0; index < count; index++) words[index] = cold_word(relocation, index);
}

static int same_node(cycle_node left, cycle_node right) {
    return left.guest == right.guest && left.epoch == right.epoch;
}

static int cycle_requires_exit(const hl_native_cache *cache, const cache_entry *source,
                               const cache_entry *target) {
    cycle_node frontier[CYCLE_FRONTIER_CAPACITY] = {{target->guest, target->instruction_epoch}};
    cycle_node destination = {source->guest, source->instruction_epoch};
    uint32_t consumed = 0, count = 1;
    int safe = source->cycle_safe != 0 && target->cycle_safe != 0;
    int closes = 0;
    while (consumed < count) {
        cycle_node current = frontier[consumed++];
        int current_slot = hl_native_cache_find(cache, current.guest);
        if (current_slot < 0 || cache->entries[current_slot].instruction_epoch != current.epoch ||
            cache->entries[current_slot].cycle_safe == 0)
            safe = 0;
        if (same_node(current, destination)) closes = 1;
        for (uint32_t index = 0; index < cache->resolved_count; ++index) {
            const resolved_relocation *edge = &cache->resolved[index];
            if (edge->generation != cache->generation || edge->source_guest != current.guest ||
                edge->source_instruction_epoch != current.epoch) continue;
            cycle_node next = {edge->relocation.target_guest, edge->relocation.target_instruction_epoch};
            uint32_t node = 0;
            while (node < count && !same_node(frontier[node], next)) node++;
            if (node < count) continue;
            if (count == CYCLE_FRONTIER_CAPACITY) return 1;
            frontier[count++] = next;
        }
    }
    return closes && !safe;
}

static hl_native_status patch(hl_native_cache *cache, const cache_entry *source,
                              const cache_entry *target, const hl_native_relocation *relocation,
                              uint32_t patched[HL_NATIVE_RELOCATION_SPAN_WORDS]) {
    uint64_t source_offset;
    int64_t displacement;
    uint32_t *word;
    uint32_t words = span_words(relocation);
    if (source->memory_mode != target->memory_mode ||
        source->authority_generation != target->authority_generation)
        return HL_NATIVE_STATE;
    if (relocation->reserved != 0 || relocation->target_instruction_count != 0 ||
        relocation->target_epoch_known > 1 || words == 0 ||
        words > HL_NATIVE_RELOCATION_SPAN_WORDS || source->code_size < words * 4u ||
        relocation->code_offset > source->code_size - words * 4u)
        return HL_NATIVE_ARGUMENT;
    source_offset = source->code_offset + relocation->code_offset;
    word = (uint32_t *)(cache->arena->writable + source_offset);
    if (!span_matches(word, relocation, cold_word(relocation, 0))) return HL_NATIVE_STATE;
    if ((target->body_offset | source_offset) % 4 != 0) return HL_NATIVE_ARGUMENT;
    displacement = target->body_offset >= source_offset
        ? (int64_t)((target->body_offset - source_offset) / 4)
        : -(int64_t)((source_offset - target->body_offset) / 4);
    if (displacement < -(1 << 25) || displacement >= (1 << 25)) {
        hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CAPACITY);
        return HL_NATIVE_CAPACITY;
    }
    for (uint32_t index = 0; index < words; index++) patched[index] = cold_word(relocation, index);
    if (words == HL_NATIVE_RELOCATION_SPAN_WORDS) {
        if (!a64_edge_admission(patched, source_offset, target)) return HL_NATIVE_ARGUMENT;
    } else {
        patched[0] = UINT32_C(0x14000000) | ((uint32_t)displacement & UINT32_C(0x03ffffff));
    }
    for (uint32_t index = 0; index < words; index++) word[index] = patched[index];
    hl_native_status status = cache->arena->memory.publish(
        cache->arena->memory.context, cache->arena->mapping.handle, source_offset, words * 4u);
    if (status != HL_NATIVE_OK) cache->poisoned = 1;
    return status;
}

static hl_native_status remember(hl_native_cache *cache, const cache_entry *source,
                                 const cache_entry *target, const hl_native_relocation *relocation,
                                 const uint32_t patched[HL_NATIVE_RELOCATION_SPAN_WORDS],
                                 uint32_t target_epoch_wildcard) {
    if (cache->resolved_count == cache->resolved_capacity) {
        hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CAPACITY);
        return HL_NATIVE_CAPACITY;
    }
    cache->resolved[cache->resolved_count++] = (resolved_relocation){
        .source_guest = source->guest,
        .source_instruction_epoch = source->instruction_epoch,
        .source_code_offset = source->code_offset,
        .source_certificate_identity = source->certificate_identity,
        .target_certificate_identity = target->certificate_identity,
        .relocation = *relocation,
        .generation = cache->generation,
        .target_epoch_wildcard = target_epoch_wildcard,
    };
    memcpy(cache->resolved[cache->resolved_count - 1].patched, patched,
           sizeof(cache->resolved[cache->resolved_count - 1].patched));
    cache->resolved[cache->resolved_count - 1].relocation.target_instruction_count =
        target->instruction_count;
    return HL_NATIVE_OK;
}

static int overlaps(const cache_entry *entry, uint64_t first, uint64_t last) {
    return entry->source_first < last && first < entry->source_last;
}

hl_native_status hl_native_cache_relocate_site(hl_native_cache *cache, const void *site,
                                               int64_t branch_delta, uint64_t target_guest,
                                               uint64_t target_epoch, uint64_t target_identity,
                                               uint32_t *patched) {
    uintptr_t rx;
    uint64_t site_offset, branch_offset;
    cache_entry *source = NULL;
    int target_slot;
    if (patched != NULL) *patched = 0;
    if (cache == NULL || site == NULL || !cache->arena->writing || (branch_delta & 3) != 0)
        return HL_NATIVE_ARGUMENT;
    rx = (uintptr_t)cache->arena->executable;
    if (cache->arena->mapping.content < 16 || (uintptr_t)site < rx ||
        (uintptr_t)site > rx + cache->arena->mapping.content - 16)
        return HL_NATIVE_STATE;
    site_offset = (uintptr_t)site - rx;
    if ((site_offset & 15u) != 0 ||
        (branch_delta < 0 && UINT64_C(0) - (uint64_t)branch_delta > site_offset) ||
        (branch_delta >= 0 && (uint64_t)branch_delta > cache->arena->mapping.content - site_offset))
        return HL_NATIVE_STATE;
    branch_offset = branch_delta < 0 ? site_offset - (UINT64_C(0) - (uint64_t)branch_delta)
                                     : site_offset + (uint64_t)branch_delta;
    if ((branch_offset & 3u) != 0 || branch_offset > cache->arena->mapping.content - 4)
        return HL_NATIVE_STATE;
    for (uint32_t index = 0; index < cache->live_count; index++) {
        cache_entry *candidate = &cache->entries[cache->live[index]];
        if (hl_native_cache_live(cache, cache->live[index]) &&
            candidate->code_offset <= branch_offset &&
            branch_offset <= candidate->code_offset + candidate->code_size - 4 &&
            candidate->code_offset <= site_offset &&
            site_offset <= candidate->code_offset + candidate->code_size - 16) {
            source = candidate;
            break;
        }
    }
    if (source == NULL) return HL_NATIVE_STATE;
    for (uint32_t index = 0; index < cache->resolved_count; index++) {
        resolved_relocation old = cache->resolved[index];
        if (old.source_guest != source->guest || old.source_instruction_epoch != source->instruction_epoch ||
            old.source_code_offset != source->code_offset ||
            old.relocation.code_offset != branch_offset - source->code_offset)
            continue;
        int old_target_slot = hl_native_cache_find(cache, old.relocation.target_guest);
        int old_target_valid = old_target_slot >= 0 &&
            cache->entries[old_target_slot].certificate_identity == old.target_certificate_identity;
        if (!old_target_valid)
            hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_INVALIDATION);
        uint32_t *word = (uint32_t *)(cache->arena->writable + branch_offset);
        if (!hot_span_matches(word, &old)) return HL_NATIVE_STATE;
        span_restore(word, &old.relocation);
        hl_native_status status = cache->arena->memory.publish(
            cache->arena->memory.context, cache->arena->mapping.handle, branch_offset,
            span_words(&old.relocation) * 4u);
        if (status != HL_NATIVE_OK) return status;
        cache->resolved[index] = cache->resolved[--cache->resolved_count];
        break;
    }
    target_slot = hl_native_cache_find(cache, target_guest);
    if (target_slot < 0 || cache->entries[target_slot].instruction_epoch != target_epoch ||
        cache->entries[target_slot].token != target_identity)
        return HL_NATIVE_STATE;
    if (cache->entries[target_slot].conditional_self_loop != 0) return HL_NATIVE_OK;
    if (cycle_requires_exit(cache, source, &cache->entries[target_slot])) {
        hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CYCLE);
        return HL_NATIVE_OK;
    }
    hl_native_relocation relocation = {
        .code_offset = branch_offset - source->code_offset,
        .target_guest = target_guest,
        .target_instruction_epoch = target_epoch,
        .target_epoch_known = 1,
        .expected = UINT32_C(0xd503201f),
    };
    uint32_t value[HL_NATIVE_RELOCATION_SPAN_WORDS];
    hl_native_status status = patch(cache, source, &cache->entries[target_slot], &relocation, value);
    if (status == HL_NATIVE_CAPACITY) return HL_NATIVE_OK;
    if (status == HL_NATIVE_OK) status = remember(cache, source, &cache->entries[target_slot], &relocation, value, 0);
    if (status == HL_NATIVE_OK && patched != NULL) *patched = 1;
    return status;
}

hl_native_status hl_native_cache_relocations_invalidate(hl_native_cache *cache,
                                                        uint64_t first, uint64_t last) {
    uint32_t retained = 0;
    if (cache == NULL || last <= first) return HL_NATIVE_ARGUMENT;
    if (cache->resolved_count != 0 && !cache->arena->writing) return HL_NATIVE_STATE;
    for (uint32_t index = 0; index < cache->relocation_count; index++) {
        pending_relocation pending = cache->relocations[index];
        int source_slot = hl_native_cache_find(cache, pending.source_guest);
        cache_entry *source = source_slot < 0 ? NULL : &cache->entries[source_slot];
        if (source != NULL && source->instruction_epoch == pending.source_instruction_epoch &&
            source->code_offset == pending.source_code_offset &&
            source->certificate_identity == pending.source_certificate_identity &&
            !overlaps(source, first, last))
            cache->relocations[retained++] = pending;
    }
    cache->relocation_count = retained;
    retained = 0;
    for (uint32_t index = 0; index < cache->resolved_count; index++) {
        resolved_relocation resolved = cache->resolved[index];
        int source_slot = hl_native_cache_find(cache, resolved.source_guest);
        int target_slot = hl_native_cache_find(cache, resolved.relocation.target_guest);
        cache_entry *source = source_slot < 0 ? NULL : &cache->entries[source_slot];
        cache_entry *target = target_slot < 0 ? NULL : &cache->entries[target_slot];
        int source_removed = source == NULL || source->instruction_epoch != resolved.source_instruction_epoch ||
            source->code_offset != resolved.source_code_offset ||
            source->certificate_identity != resolved.source_certificate_identity || overlaps(source, first, last);
        int target_removed = target == NULL ||
            target->instruction_epoch != resolved.relocation.target_instruction_epoch ||
            target->certificate_identity != resolved.target_certificate_identity || overlaps(target, first, last);
        if (!source_removed && !target_removed) {
            cache->resolved[retained++] = resolved;
            continue;
        }
        uint32_t words = span_words(&resolved.relocation);
        if (source != NULL && source->certificate_identity == resolved.source_certificate_identity &&
            words <= HL_NATIVE_RELOCATION_SPAN_WORDS &&
            source->code_size >= words * 4u &&
            resolved.relocation.code_offset <= source->code_size - words * 4u) {
            uint64_t offset = source->code_offset + resolved.relocation.code_offset;
            uint32_t *word = (uint32_t *)(cache->arena->writable + offset);
            if (!hot_span_matches(word, &resolved)) return HL_NATIVE_STATE;
            span_restore(word, &resolved.relocation);
            hl_native_status status = cache->arena->memory.publish(
                cache->arena->memory.context, cache->arena->mapping.handle, offset, words * 4u);
            if (status != HL_NATIVE_OK) {
                cache->poisoned = 1;
                return status;
            }
            hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_INVALIDATION);
        }
        if (!source_removed && target_removed) {
            if (cache->relocation_count == cache->relocation_capacity) {
                hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CAPACITY);
                return HL_NATIVE_CAPACITY;
            }
            hl_native_relocation pending = resolved.relocation;
            if (resolved.target_epoch_wildcard) {
                pending.target_instruction_epoch = 0;
                pending.target_epoch_known = 0;
            }
            pending.target_instruction_count = 0;
            cache->relocations[cache->relocation_count++] = (pending_relocation){
                .source_guest = resolved.source_guest,
                .source_instruction_epoch = resolved.source_instruction_epoch,
                .source_code_offset = resolved.source_code_offset,
                .source_certificate_identity = resolved.source_certificate_identity,
                .relocation = pending,
                .generation = cache->generation,
            };
        }
    }
    cache->resolved_count = retained;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_cache_relocate(hl_native_cache *cache, uint64_t source_guest,
                                          uint64_t source_instruction_epoch,
                                          const hl_native_relocation *relocations, uint32_t count) {
    int source_slot;
    if (cache == NULL || relocations == NULL || count == 0 || count > cache->relocation_capacity)
        return HL_NATIVE_ARGUMENT;
    if (!cache->arena->writing) return HL_NATIVE_STATE;
    source_slot = hl_native_cache_find(cache, source_guest);
    if (source_slot < 0) return HL_NATIVE_STATE;
    cache_entry *source = &cache->entries[source_slot];
    if (source->instruction_epoch != source_instruction_epoch) return HL_NATIVE_STATE;
    for (uint32_t index = 0; index < count; index++) {
        const hl_native_relocation *relocation = &relocations[index];
        int target_slot = hl_native_cache_find(cache, relocation->target_guest);
        if (target_slot >= 0 && cache->entries[target_slot].conditional_self_loop != 0)
            continue;
        if (target_slot >= 0 && cycle_requires_exit(cache, source, &cache->entries[target_slot])) {
            hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CYCLE);
            continue;
        }
        if (target_slot >= 0 &&
            (!relocation->target_epoch_known ||
             cache->entries[target_slot].instruction_epoch == relocation->target_instruction_epoch)) {
            hl_native_relocation bound = *relocation;
            bound.target_instruction_epoch = cache->entries[target_slot].instruction_epoch;
            bound.target_epoch_known = 1;
            uint32_t patched[HL_NATIVE_RELOCATION_SPAN_WORDS];
            if (cache->resolved_count == cache->resolved_capacity) {
                hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CAPACITY);
                return HL_NATIVE_CAPACITY;
            }
            hl_native_status status = patch(cache, source, &cache->entries[target_slot], &bound, patched);
            if (status == HL_NATIVE_OK)
                status = remember(cache, source, &cache->entries[target_slot], &bound, patched,
                                  !relocation->target_epoch_known);
            if (status != HL_NATIVE_OK) return status;
            continue;
        }
        if (cache->relocation_count == cache->relocation_capacity) {
            hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CAPACITY);
            return HL_NATIVE_CAPACITY;
        }
        cache->relocations[cache->relocation_count++] = (pending_relocation){
            .source_guest = source_guest,
            .source_instruction_epoch = source_instruction_epoch,
            .source_code_offset = source->code_offset,
            .source_certificate_identity = source->certificate_identity,
            .relocation = *relocation,
            .generation = cache->generation,
        };
        hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_COLD_TARGET);
    }
    return HL_NATIVE_OK;
}

hl_native_status hl_native_cache_resolve(hl_native_cache *cache, uint64_t target_guest,
                                         uint64_t target_instruction_epoch) {
    int target_slot;
    uint32_t retained = 0;
    if (cache == NULL) return HL_NATIVE_ARGUMENT;
    if (!cache->arena->writing) return HL_NATIVE_STATE;
    target_slot = hl_native_cache_find(cache, target_guest);
    if (target_slot < 0 || cache->entries[target_slot].instruction_epoch != target_instruction_epoch)
        return HL_NATIVE_STATE;
    if (cache->entries[target_slot].conditional_self_loop != 0) {
        for (uint32_t index = 0; index < cache->relocation_count; index++) {
            pending_relocation pending = cache->relocations[index];
            if (pending.generation != cache->generation ||
                pending.relocation.target_guest == target_guest)
                continue;
            cache->relocations[retained++] = pending;
        }
        cache->relocation_count = retained;
        return HL_NATIVE_OK;
    }
    for (uint32_t index = 0; index < cache->relocation_count; index++) {
        pending_relocation pending = cache->relocations[index];
        if (pending.generation != cache->generation) continue;
        if (pending.relocation.target_guest != target_guest ||
            (pending.relocation.target_epoch_known &&
             pending.relocation.target_instruction_epoch != target_instruction_epoch)) {
            cache->relocations[retained++] = pending;
            continue;
        }
        int source_slot = hl_native_cache_find(cache, pending.source_guest);
        if (source_slot < 0 || cache->entries[source_slot].instruction_epoch != pending.source_instruction_epoch ||
            cache->entries[source_slot].code_offset != pending.source_code_offset ||
            cache->entries[source_slot].certificate_identity != pending.source_certificate_identity)
            continue;
        if (cycle_requires_exit(cache, &cache->entries[source_slot], &cache->entries[target_slot])) {
            hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CYCLE);
            continue;
        }
        if (cache->resolved_count == cache->resolved_capacity) {
            hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_CAPACITY);
            return HL_NATIVE_CAPACITY;
        }
        hl_native_relocation bound = pending.relocation;
        bound.target_instruction_epoch = target_instruction_epoch;
        bound.target_epoch_known = 1;
        uint32_t patched[HL_NATIVE_RELOCATION_SPAN_WORDS];
        hl_native_status status = patch(cache, &cache->entries[source_slot],
                                        &cache->entries[target_slot], &bound, patched);
        if (status == HL_NATIVE_OK)
            status = remember(cache, &cache->entries[source_slot], &cache->entries[target_slot], &bound, patched,
                              !pending.relocation.target_epoch_known);
        if (status != HL_NATIVE_OK) return status;
    }
    cache->relocation_count = retained;
    return HL_NATIVE_OK;
}

void hl_native_cache_relocations_clear(hl_native_cache *cache) {
    if (cache != NULL) {
        cache->relocation_count = 0;
        cache->resolved_count = 0;
    }
}

hl_native_status hl_native_cache_relocations_restore(hl_native_cache *cache) {
    if (cache == NULL) return HL_NATIVE_ARGUMENT;
    if (cache->resolved_count != 0 && !cache->arena->writing) {
        hl_native_cache_fail(cache);
        return HL_NATIVE_STATE;
    }
    while (cache->resolved_count != 0) {
        resolved_relocation *resolved = &cache->resolved[cache->resolved_count - 1];
        int source_slot = hl_native_cache_find(cache, resolved->source_guest);
        cache_entry *source = source_slot < 0 ? NULL : &cache->entries[source_slot];
        uint32_t words = span_words(&resolved->relocation);
        if (source == NULL || source->instruction_epoch != resolved->source_instruction_epoch ||
            source->code_offset != resolved->source_code_offset ||
            source->certificate_identity != resolved->source_certificate_identity ||
            words > HL_NATIVE_RELOCATION_SPAN_WORDS || source->code_size < words * 4u ||
            resolved->relocation.code_offset > source->code_size - words * 4u)
            {
                hl_native_cache_fail(cache);
                return HL_NATIVE_STATE;
            }
        uint64_t offset = source->code_offset + resolved->relocation.code_offset;
        uint32_t *word = (uint32_t *)(cache->arena->writable + offset);
        if (!hot_span_matches(word, resolved)) {
            hl_native_cache_fail(cache);
            return HL_NATIVE_STATE;
        }
        span_restore(word, &resolved->relocation);
        hl_native_status status = cache->arena->memory.publish(
            cache->arena->memory.context, cache->arena->mapping.handle, offset, words * 4u);
        if (status != HL_NATIVE_OK) {
            hl_native_cache_fail(cache);
            return status;
        }
        cache->resolved_count--;
        hl_native_cache_observe(cache, HL_NATIVE_CACHE_RELOCATION_INVALIDATION);
    }
    cache->relocation_count = 0;
    return HL_NATIVE_OK;
}
