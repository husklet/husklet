#include "private.h"

/* Unguarded cycles must retain one typed dispatcher exit. Publication cannot
 * allocate, so admission uses a fixed nonrecursive frontier. A guarded cycle
 * may close only when every examined entry has its own interrupt and budget
 * checkpoint. Saturation conservatively retains the original exit. */
enum { CYCLE_FRONTIER_CAPACITY = 64 };

typedef struct cycle_node { uint64_t guest, epoch; } cycle_node;

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
                              uint32_t *patched) {
    uint64_t source_offset;
    int64_t displacement;
    uint32_t *word;
    if (source->memory_mode != target->memory_mode ||
        source->authority_generation != target->authority_generation)
        return HL_NATIVE_STATE;
    if (relocation->reserved != 0 || relocation->target_epoch_known > 1 || source->code_size < 4 ||
        relocation->code_offset > source->code_size - 4)
        return HL_NATIVE_ARGUMENT;
    source_offset = source->code_offset + relocation->code_offset;
    word = (uint32_t *)(cache->arena->writable + source_offset);
    if (*word != relocation->expected) return HL_NATIVE_STATE;
    if ((target->body_offset | source_offset) % 4 != 0) return HL_NATIVE_ARGUMENT;
    displacement = target->body_offset >= source_offset
        ? (int64_t)((target->body_offset - source_offset) / 4)
        : -(int64_t)((source_offset - target->body_offset) / 4);
    if (displacement < -(1 << 25) || displacement >= (1 << 25)) return HL_NATIVE_CAPACITY;
    *patched = UINT32_C(0x14000000) | ((uint32_t)displacement & UINT32_C(0x03ffffff));
    *word = *patched;
    hl_native_status status = cache->arena->memory.publish(
        cache->arena->memory.context, cache->arena->mapping.handle, source_offset, 4);
    if (status != HL_NATIVE_OK) cache->poisoned = 1;
    return status;
}

static hl_native_status remember(hl_native_cache *cache, const cache_entry *source,
                                 const hl_native_relocation *relocation, uint32_t patched,
                                 uint32_t target_epoch_wildcard) {
    if (cache->resolved_count == cache->resolved_capacity) return HL_NATIVE_CAPACITY;
    cache->resolved[cache->resolved_count++] = (resolved_relocation){
        source->guest, source->instruction_epoch, source->code_offset,
        *relocation, patched, cache->generation, target_epoch_wildcard};
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
        uint32_t *word = (uint32_t *)(cache->arena->writable + branch_offset);
        if (*word != old.patched) return HL_NATIVE_STATE;
        *word = old.relocation.expected;
        hl_native_status status = cache->arena->memory.publish(
            cache->arena->memory.context, cache->arena->mapping.handle, branch_offset, 4);
        if (status != HL_NATIVE_OK) return status;
        cache->resolved[index] = cache->resolved[--cache->resolved_count];
        break;
    }
    target_slot = hl_native_cache_find(cache, target_guest);
    if (target_slot < 0 || cache->entries[target_slot].instruction_epoch != target_epoch ||
        cache->entries[target_slot].token != target_identity)
        return HL_NATIVE_STATE;
    if (cache->entries[target_slot].conditional_self_loop != 0) return HL_NATIVE_OK;
    if (cycle_requires_exit(cache, source, &cache->entries[target_slot])) return HL_NATIVE_OK;
    hl_native_relocation relocation = {
        .code_offset = branch_offset - source->code_offset,
        .target_guest = target_guest,
        .target_instruction_epoch = target_epoch,
        .target_epoch_known = 1,
        .expected = UINT32_C(0xd503201f),
    };
    uint32_t value;
    hl_native_status status = patch(cache, source, &cache->entries[target_slot], &relocation, &value);
    if (status == HL_NATIVE_CAPACITY) return HL_NATIVE_OK;
    if (status == HL_NATIVE_OK) status = remember(cache, source, &relocation, value, 0);
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
            source->code_offset == pending.source_code_offset && !overlaps(source, first, last))
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
            source->code_offset != resolved.source_code_offset || overlaps(source, first, last);
        int target_removed = target == NULL ||
            target->instruction_epoch != resolved.relocation.target_instruction_epoch || overlaps(target, first, last);
        if (!source_removed && !target_removed) {
            cache->resolved[retained++] = resolved;
            continue;
        }
        if (source != NULL && source->code_size >= 4 &&
            resolved.relocation.code_offset <= source->code_size - 4) {
            uint64_t offset = source->code_offset + resolved.relocation.code_offset;
            uint32_t *word = (uint32_t *)(cache->arena->writable + offset);
            if (*word != resolved.patched) return HL_NATIVE_STATE;
            *word = resolved.relocation.expected;
            hl_native_status status = cache->arena->memory.publish(
                cache->arena->memory.context, cache->arena->mapping.handle, offset, 4);
            if (status != HL_NATIVE_OK) {
                cache->poisoned = 1;
                return status;
            }
        }
        if (!source_removed && target_removed) {
            if (cache->relocation_count == cache->relocation_capacity) return HL_NATIVE_CAPACITY;
            hl_native_relocation pending = resolved.relocation;
            if (resolved.target_epoch_wildcard) {
                pending.target_instruction_epoch = 0;
                pending.target_epoch_known = 0;
            }
            cache->relocations[cache->relocation_count++] = (pending_relocation){
                resolved.source_guest, resolved.source_instruction_epoch,
                resolved.source_code_offset, pending, cache->generation};
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
        if (target_slot >= 0 && cycle_requires_exit(cache, source, &cache->entries[target_slot]))
            continue;
        if (target_slot >= 0 &&
            (!relocation->target_epoch_known ||
             cache->entries[target_slot].instruction_epoch == relocation->target_instruction_epoch)) {
            hl_native_relocation bound = *relocation;
            bound.target_instruction_epoch = cache->entries[target_slot].instruction_epoch;
            bound.target_epoch_known = 1;
            uint32_t patched;
            if (cache->resolved_count == cache->resolved_capacity) return HL_NATIVE_CAPACITY;
            hl_native_status status = patch(cache, source, &cache->entries[target_slot], &bound, &patched);
            if (status == HL_NATIVE_OK)
                status = remember(cache, source, &bound, patched, !relocation->target_epoch_known);
            if (status != HL_NATIVE_OK) return status;
            continue;
        }
        if (cache->relocation_count == cache->relocation_capacity) return HL_NATIVE_CAPACITY;
        cache->relocations[cache->relocation_count++] = (pending_relocation){
            source_guest, source_instruction_epoch, source->code_offset, *relocation, cache->generation};
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
            cache->entries[source_slot].code_offset != pending.source_code_offset)
            continue;
        if (cycle_requires_exit(cache, &cache->entries[source_slot], &cache->entries[target_slot]))
            continue;
        if (cache->resolved_count == cache->resolved_capacity) return HL_NATIVE_CAPACITY;
        hl_native_relocation bound = pending.relocation;
        bound.target_instruction_epoch = target_instruction_epoch;
        bound.target_epoch_known = 1;
        uint32_t patched;
        hl_native_status status = patch(cache, &cache->entries[source_slot],
                                        &cache->entries[target_slot], &bound, &patched);
        if (status == HL_NATIVE_OK)
            status = remember(cache, &cache->entries[source_slot], &bound, patched,
                              !pending.relocation.target_epoch_known);
        if (status != HL_NATIVE_OK) return status;
    }
    cache->relocation_count = retained;
    return HL_NATIVE_OK;
}

void hl_native_cache_relocations_clear(hl_native_cache *cache) {
    if (cache != NULL) cache->relocation_count = 0;
}
