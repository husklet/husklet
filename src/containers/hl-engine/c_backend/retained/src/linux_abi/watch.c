#include "watch.h"

#include <pthread.h>
#include <stdlib.h>
#include <string.h>

typedef struct watch_slot {
    uint64_t device;
    uint64_t object;
    uint64_t size;
    uint64_t pending_size;
    uint32_t generation;
    uint32_t references;
    uint32_t pending_flags;
    uint32_t queued;
} watch_slot;

typedef struct watch_state {
    pthread_mutex_t lock;
    watch_slot *slots;
    uint32_t *queue;
    size_t count;
    size_t capacity;
    size_t queued;
    size_t queue_capacity;
    int shutdown;
} watch_state;

static uint64_t watch_token(size_t index, uint32_t generation) {
    return ((uint64_t)generation << 32) | ((uint64_t)index + 1u);
}

static watch_slot *watch_lookup(watch_state *state, uint64_t token, size_t *index) {
    uint32_t low = (uint32_t)token;
    size_t position;
    if (low == 0) return NULL;
    position = (size_t)(low - 1u);
    if (position >= state->count || state->slots[position].references == 0 ||
        state->slots[position].generation != (uint32_t)(token >> 32))
        return NULL;
    if (index != NULL) *index = position;
    return &state->slots[position];
}

hl_status hl_linux_watch_init(hl_linux_watch_set *set) {
    watch_state *state;
    if (set == NULL || set->state != NULL) return HL_STATUS_INVALID_ARGUMENT;
    state = calloc(1, sizeof(*state));
    if (state == NULL) return HL_STATUS_OUT_OF_MEMORY;
    if (pthread_mutex_init(&state->lock, NULL) != 0) {
        free(state);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    set->state = state;
    return HL_STATUS_OK;
}

void hl_linux_watch_close(hl_linux_watch_set *set) {
    watch_state *state;
    if (set == NULL || set->state == NULL) return;
    state = set->state;
    pthread_mutex_destroy(&state->lock);
    free(state->slots);
    free(state->queue);
    free(state);
    set->state = NULL;
}

hl_status hl_linux_watch_retain(hl_linux_watch_set *set, uint64_t device, uint64_t object, uint64_t size,
                                uint64_t *token, int *created) {
    watch_state *state;
    size_t index, free_index = SIZE_MAX;
    watch_slot *slot;
    if (set == NULL || set->state == NULL || token == NULL || created == NULL) return HL_STATUS_INVALID_ARGUMENT;
    state = set->state;
    pthread_mutex_lock(&state->lock);
    if (state->shutdown) {
        pthread_mutex_unlock(&state->lock);
        return HL_STATUS_INTERRUPTED;
    }
    for (index = 0; index < state->count; ++index) {
        slot = &state->slots[index];
        if (slot->references != 0 && slot->device == device && slot->object == object) {
            if (slot->references == UINT32_MAX) {
                pthread_mutex_unlock(&state->lock);
                return HL_STATUS_RESOURCE_LIMIT;
            }
            slot->references++;
            *token = watch_token(index, slot->generation);
            *created = 0;
            pthread_mutex_unlock(&state->lock);
            return HL_STATUS_OK;
        }
        if (slot->references == 0 && free_index == SIZE_MAX) free_index = index;
    }
    if (free_index == SIZE_MAX) {
        if (state->count == UINT32_MAX) {
            pthread_mutex_unlock(&state->lock);
            return HL_STATUS_RESOURCE_LIMIT;
        }
        if (state->count == state->capacity) {
            size_t capacity = state->capacity == 0 ? 8u : state->capacity * 2u;
            watch_slot *grown = realloc(state->slots, capacity * sizeof(*grown));
            if (grown == NULL) {
                pthread_mutex_unlock(&state->lock);
                return HL_STATUS_OUT_OF_MEMORY;
            }
            memset(grown + state->capacity, 0, (capacity - state->capacity) * sizeof(*grown));
            state->slots = grown;
            state->capacity = capacity;
        }
        free_index = state->count++;
    }
    slot = &state->slots[free_index];
    slot->generation++;
    if (slot->generation == 0) slot->generation = 1;
    slot->device = device;
    slot->object = object;
    slot->size = size;
    slot->pending_size = size;
    slot->references = 1;
    slot->pending_flags = 0;
    slot->queued = 0;
    *token = watch_token(free_index, slot->generation);
    *created = 1;
    pthread_mutex_unlock(&state->lock);
    return HL_STATUS_OK;
}

hl_status hl_linux_watch_release(hl_linux_watch_set *set, uint64_t token, int *removed) {
    watch_state *state;
    watch_slot *slot;
    if (set == NULL || set->state == NULL || removed == NULL) return HL_STATUS_INVALID_ARGUMENT;
    state = set->state;
    pthread_mutex_lock(&state->lock);
    slot = watch_lookup(state, token, NULL);
    if (slot == NULL) {
        pthread_mutex_unlock(&state->lock);
        return HL_STATUS_NOT_FOUND;
    }
    slot->references--;
    *removed = slot->references == 0;
    if (*removed) {
        slot->queued = 0;
        slot->pending_flags = 0;
    }
    pthread_mutex_unlock(&state->lock);
    return HL_STATUS_OK;
}

hl_status hl_linux_watch_enqueue(hl_linux_watch_set *set, uint64_t token, uint64_t size, uint32_t flags) {
    watch_state *state;
    watch_slot *slot;
    size_t index;
    if (set == NULL || set->state == NULL) return HL_STATUS_INVALID_ARGUMENT;
    state = set->state;
    pthread_mutex_lock(&state->lock);
    slot = watch_lookup(state, token, &index);
    if (slot == NULL || state->shutdown) {
        pthread_mutex_unlock(&state->lock);
        return slot == NULL ? HL_STATUS_NOT_FOUND : HL_STATUS_INTERRUPTED;
    }
    slot->pending_size = size;
    slot->pending_flags |= flags;
    if (!slot->queued) {
        if (state->queued == state->queue_capacity) {
            size_t capacity = state->queue_capacity == 0 ? 8u : state->queue_capacity * 2u;
            uint32_t *grown = realloc(state->queue, capacity * sizeof(*grown));
            if (grown == NULL) {
                pthread_mutex_unlock(&state->lock);
                return HL_STATUS_OUT_OF_MEMORY;
            }
            state->queue = grown;
            state->queue_capacity = capacity;
        }
        state->queue[state->queued++] = (uint32_t)index;
        slot->queued = 1;
    }
    pthread_mutex_unlock(&state->lock);
    return HL_STATUS_OK;
}

hl_status hl_linux_watch_drain(hl_linux_watch_set *set, hl_linux_watch_change_fn callback, void *opaque,
                               size_t *out_count) {
    watch_state *state;
    hl_linux_watch_change *changes;
    size_t index, count = 0;
    if (set == NULL || set->state == NULL || callback == NULL || out_count == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *out_count = 0;
    state = set->state;
    pthread_mutex_lock(&state->lock);
    changes = state->queued == 0 ? NULL : calloc(state->queued, sizeof(*changes));
    if (state->queued != 0 && changes == NULL) {
        pthread_mutex_unlock(&state->lock);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    for (index = 0; index < state->queued; ++index) {
        size_t position = state->queue[index];
        watch_slot *slot = &state->slots[position];
        if (!slot->queued || slot->references == 0) continue;
        changes[count++] = (hl_linux_watch_change){watch_token(position, slot->generation),
                                                   slot->device,
                                                   slot->object,
                                                   slot->size,
                                                   slot->pending_size,
                                                   slot->pending_flags};
        slot->size = slot->pending_size;
        slot->pending_flags = 0;
        slot->queued = 0;
    }
    state->queued = 0;
    pthread_mutex_unlock(&state->lock);
    for (index = 0; index < count; ++index)
        callback(opaque, &changes[index]);
    free(changes);
    *out_count = count;
    return HL_STATUS_OK;
}

void hl_linux_watch_shutdown(hl_linux_watch_set *set) {
    watch_state *state;
    if (set == NULL || set->state == NULL) return;
    state = set->state;
    pthread_mutex_lock(&state->lock);
    state->shutdown = 1;
    pthread_mutex_unlock(&state->lock);
}

hl_status hl_linux_watch_fork_prepare(hl_linux_watch_set *set, hl_linux_watch_fork_plan *plan) {
    watch_state *state;
    size_t index;
    if (set == NULL || set->state == NULL || plan == NULL || (plan->capacity != 0 && plan->records == NULL))
        return HL_STATUS_INVALID_ARGUMENT;
    state = set->state;
    pthread_mutex_lock(&state->lock);
    plan->count = 0;
    for (index = 0; index < state->count; ++index) {
        watch_slot *slot = &state->slots[index];
        if (slot->references == 0) continue;
        if (plan->count == plan->capacity) {
            pthread_mutex_unlock(&state->lock);
            return HL_STATUS_RESOURCE_LIMIT;
        }
        plan->records[plan->count++] =
            (hl_linux_watch_fork_record){watch_token(index, slot->generation), slot->device, slot->object};
    }
    return HL_STATUS_OK;
}

hl_status hl_linux_watch_fork_snapshot(hl_linux_watch_set *set, hl_linux_watch_fork_plan *plan) {
    watch_state *state;
    size_t index;
    pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
    if (set == NULL || set->state == NULL || plan == NULL || (plan->capacity != 0 && plan->records == NULL))
        return HL_STATUS_INVALID_ARGUMENT;
    state = set->state;
    /* The caller proved there is no live owner.  Replacing the bytes also
       avoids depending on a libc's post-fork representation of an unlocked
       process-private mutex. */
    memcpy(&state->lock, &fresh, sizeof(fresh));
    plan->count = 0;
    for (index = 0; index < state->count; ++index) {
        watch_slot *slot = &state->slots[index];
        if (slot->references == 0) continue;
        if (plan->count == plan->capacity) return HL_STATUS_RESOURCE_LIMIT;
        plan->records[plan->count++] =
            (hl_linux_watch_fork_record){watch_token(index, slot->generation), slot->device, slot->object};
    }
    return HL_STATUS_OK;
}

void hl_linux_watch_fork_parent(hl_linux_watch_set *set) {
    if (set != NULL && set->state != NULL) pthread_mutex_unlock(&((watch_state *)set->state)->lock);
}

hl_status hl_linux_watch_fork_child(hl_linux_watch_set *set, hl_linux_watch_fork_plan *plan,
                                    hl_linux_watch_rebuild_fn rebuild, void *opaque) {
    watch_state *state;
    size_t index;
    pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
    if (set == NULL || set->state == NULL || plan == NULL || (plan->count != 0 && plan->records == NULL))
        return HL_STATUS_INVALID_ARGUMENT;
    state = set->state;
    state->queued = 0;
    state->shutdown = 0;
    for (index = 0; index < state->count; ++index) {
        watch_slot *slot = &state->slots[index];
        slot->queued = 0;
        slot->pending_flags = 0;
        slot->pending_size = slot->size;
        if (slot->references == 0) continue;
        slot->generation++;
        if (slot->generation == 0) slot->generation = 1;
    }
    /* Only the fork-preparing thread owned this mutex. Replace the inherited
       locked bytes without calling pthread or allocator code in the child. */
    memcpy(&state->lock, &fresh, sizeof(fresh));
    for (index = 0; index < plan->count; ++index) {
        uint32_t low = (uint32_t)plan->records[index].token;
        size_t position = low == 0 ? SIZE_MAX : (size_t)(low - 1u);
        if (position >= state->count || state->slots[position].references == 0) return HL_STATUS_PLATFORM_FAILURE;
        plan->records[index].token = watch_token(position, state->slots[position].generation);
    }
    if (rebuild != NULL)
        for (index = 0; index < plan->count; ++index)
            rebuild(opaque, plan->records[index].token, plan->records[index].device, plan->records[index].object);
    return HL_STATUS_OK;
}
