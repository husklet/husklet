#ifndef HL_LINUX_ABI_SENTRY_SNAPSHOT_H
#define HL_LINUX_ABI_SENTRY_SNAPSHOT_H

#include <errno.h>
#include <stdint.h>
#include <string.h>

struct hl_sentry_snapshot {
    uint64_t handle;
    int32_t owner;
    uint32_t token;
    uint16_t payload;
    uint8_t active;
};

struct hl_sentry_snapshots {
    struct hl_sentry_snapshot *slot;
    uint32_t count;
    uint64_t generation;
};

static inline struct hl_sentry_snapshot *hl_sentry_snapshot_find(
    struct hl_sentry_snapshots *snapshots, int32_t owner, uint32_t token, uint64_t handle) {
    uint32_t encoded = (uint32_t)(handle & 0xffu);
    if (encoded == 0 || encoded > snapshots->count) return NULL;
    struct hl_sentry_snapshot *snapshot = &snapshots->slot[encoded - 1u];
    if (!snapshot->active || snapshot->handle != handle || snapshot->owner != owner ||
        snapshot->token != token)
        return NULL;
    return snapshot;
}

static inline int64_t hl_sentry_snapshot_reserve(
    struct hl_sentry_snapshots *snapshots, int32_t owner, uint32_t token, uint16_t payload) {
    uint32_t index = 0;
    while (index < snapshots->count && snapshots->slot[index].active) index++;
    if (index == snapshots->count) return -EAGAIN;

    uint64_t generation = ++snapshots->generation & ((uint64_t)INT64_MAX >> 8);
    if (generation == 0)
        generation = ++snapshots->generation & ((uint64_t)INT64_MAX >> 8);
    uint64_t handle = generation << 8 | (uint64_t)(index + 1u);
    snapshots->slot[index] = (struct hl_sentry_snapshot){
        .handle = handle,
        .owner = owner,
        .token = token,
        .payload = payload,
        .active = 1,
    };
    return (int64_t)handle;
}

static inline int hl_sentry_snapshot_take(
    struct hl_sentry_snapshots *snapshots, int32_t owner, uint32_t token, uint64_t handle,
    uint16_t *payload) {
    struct hl_sentry_snapshot *snapshot =
        hl_sentry_snapshot_find(snapshots, owner, token, handle);
    if (!snapshot) return -EINVAL;
    *payload = snapshot->payload;
    memset(snapshot, 0, sizeof *snapshot);
    return 0;
}

static inline int hl_sentry_snapshot_take_owner(
    struct hl_sentry_snapshots *snapshots, int32_t owner, uint16_t *payload) {
    for (uint32_t i = 0; i < snapshots->count; i++)
        if (snapshots->slot[i].active && snapshots->slot[i].owner == owner) {
            *payload = snapshots->slot[i].payload;
            memset(&snapshots->slot[i], 0, sizeof snapshots->slot[i]);
            return 1;
        }
    return 0;
}

#endif
