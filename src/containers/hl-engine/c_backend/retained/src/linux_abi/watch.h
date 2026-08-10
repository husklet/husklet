#ifndef HL_LINUX_WATCH_H
#define HL_LINUX_WATCH_H

#include "hl/host_services.h"

#include <stddef.h>
#include <stdint.h>

typedef struct hl_linux_watch_set {
    void *state;
} hl_linux_watch_set;

typedef struct hl_linux_watch_change {
    uint64_t token;
    uint64_t device;
    uint64_t object;
    uint64_t old_size;
    uint64_t new_size;
    uint32_t flags;
} hl_linux_watch_change;

typedef void (*hl_linux_watch_change_fn)(void *opaque, const hl_linux_watch_change *change);
typedef void (*hl_linux_watch_rebuild_fn)(void *opaque, uint64_t token, uint64_t device, uint64_t object);

typedef struct hl_linux_watch_fork_record {
    uint64_t token;
    uint64_t device;
    uint64_t object;
} hl_linux_watch_fork_record;

typedef struct hl_linux_watch_fork_plan {
    hl_linux_watch_fork_record *records;
    size_t capacity;
    size_t count;
} hl_linux_watch_fork_plan;

hl_status hl_linux_watch_init(hl_linux_watch_set *set);
void hl_linux_watch_close(hl_linux_watch_set *set);
hl_status hl_linux_watch_retain(hl_linux_watch_set *set, uint64_t device, uint64_t object, uint64_t size,
                                uint64_t *token, int *created);
hl_status hl_linux_watch_release(hl_linux_watch_set *set, uint64_t token, int *removed);
hl_status hl_linux_watch_enqueue(hl_linux_watch_set *set, uint64_t token, uint64_t size, uint32_t flags);
hl_status hl_linux_watch_drain(hl_linux_watch_set *set, hl_linux_watch_change_fn callback, void *opaque,
                               size_t *out_count);
void hl_linux_watch_shutdown(hl_linux_watch_set *set);

/* Called around fork in the thread which invokes fork. */
hl_status hl_linux_watch_fork_prepare(hl_linux_watch_set *set, hl_linux_watch_fork_plan *plan);
/* Snapshot after the owner has joined every producer/consumer and excluded
   retain/release.  Unlike fork_prepare this does not carry a locked pthread
   mutex across fork. */
hl_status hl_linux_watch_fork_snapshot(hl_linux_watch_set *set, hl_linux_watch_fork_plan *plan);
void hl_linux_watch_fork_parent(hl_linux_watch_set *set);
hl_status hl_linux_watch_fork_child(hl_linux_watch_set *set, hl_linux_watch_fork_plan *plan,
                                    hl_linux_watch_rebuild_fn rebuild, void *opaque);

#endif
