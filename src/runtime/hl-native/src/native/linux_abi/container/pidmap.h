#ifndef HL_LINUX_ABI_CONTAINER_PIDMAP_H
#define HL_LINUX_ABI_CONTAINER_PIDMAP_H

#include <stdint.h>
#include <stddef.h>

#define HL_LINUX_PIDMAP_CAPACITY 4096

/* A container is ONE pid namespace, but it is served by SEVERAL engine launches: the spec tree's top
 * process and one more for every exec session, each a separate host process forked out of the container
 * daemon rather than out of the init. A per-launch registry therefore hands every launch guest 1 and then
 * allocates 2, 3, 4 ... independently, so two live processes in one container answer the same guest pid --
 * and since a capture files each process's image under `proc.<guest pid>`, two of them claimed one image
 * group and the whole capture was refused.
 *
 * The registry is consequently placed in the one shared object every launch of a container already
 * inherits: the checkpoint trigger. Its generation word keeps offset 0 (ckpt_poll maps exactly those four
 * bytes); the identity registry lives one page in, in a region reserved for it. The reservation is a fixed
 * size rather than sizeof(storage) so the file's layout is not a private detail of pidmap.c; the static
 * assertion beside the storage definition keeps the two honest. */
#define HL_LINUX_IDENTITY_REGISTRY_MINIMUM_OFFSET 4096u
#define HL_LINUX_IDENTITY_REGISTRY_BYTES (1u << 21)

/* The byte offset the registry region actually starts at: the reservation above, rounded UP to the host's
 * mmap allocation granularity. mmap rejects an unaligned offset with EINVAL, and the two hosts disagree on
 * that granularity -- Linux pages this tree at 4096, macOS on Apple silicon at 16384. A hard 4096 mapped on
 * Linux and failed on macOS, and it failed SILENTLY: the launch fell back to a process-local registry, so
 * every launch of one container allocated guest 1, 2, 3, 4 over again and two live processes claimed the
 * same `proc.<guest pid>` image group. Every user of the shared object -- the ftruncate that sizes it and
 * the mmap that maps it -- must ask this function rather than reading the reservation macro. */
uint64_t hl_linux_identity_registry_offset(void);

typedef struct hl_linux_pidmap_entry {
    int32_t guest;
    int32_t host;
} hl_linux_pidmap_entry;

typedef struct hl_linux_pidmap_storage hl_linux_pidmap_storage;
typedef struct hl_linux_identity_registry_storage hl_linux_identity_registry_storage;

typedef struct hl_linux_identity_registry {
    hl_linux_identity_registry_storage *storage;
    struct hl_linux_pidmap *map[3];
    int lock_fd;
} hl_linux_identity_registry;

typedef struct hl_linux_pidmap {
    hl_linux_pidmap_storage *storage;
    hl_linux_identity_registry *registry;
    uint32_t kind;
    int active;
} hl_linux_pidmap;

typedef struct hl_linux_pidmap_update {
    hl_linux_pidmap *map;
    int32_t guest;
    int32_t host;
} hl_linux_pidmap_update;

void hl_linux_pidmap_init(hl_linux_pidmap *map);
int hl_linux_pidmap_prepare_shared(hl_linux_pidmap *map);
int hl_linux_identity_registry_prepare(hl_linux_identity_registry *registry, hl_linux_pidmap *pid,
                                       hl_linux_pidmap *pgid, hl_linux_pidmap *sid);
/* Prepare the registry over a descriptor shared by every engine launch of one container (the checkpoint
 * trigger). `descriptor` is duplicated, so the caller keeps ownership of the one it passed. The first
 * launch to arrive initializes the region under the same file lock every writer takes; the rest adopt it. */
int hl_linux_identity_registry_prepare_shared_descriptor(hl_linux_identity_registry *registry, int descriptor,
                                                         hl_linux_pidmap *pid, hl_linux_pidmap *pgid,
                                                         hl_linux_pidmap *sid);
/* Enter this launch into the container's namespace and return ITS OWN guest pid, publishing the process,
 * group and session identities in one transaction. The first launch to arrive is the namespace init and
 * takes guest 1 with no parent inside the namespace (`*out_guest_parent` = 0); every later launch is an
 * exec session, which Linux gives an ordinary namespace pid whose parent lies outside the namespace, so it
 * takes the next free guest pid and reports init as its parent. Idempotent for a host process already
 * published. Returns -1 on failure. */
int32_t hl_linux_identity_registry_join(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, hl_linux_pidmap *sid,
                                        int32_t host_process, int32_t host_group, int32_t host_session,
                                        int32_t *out_guest_parent);
int hl_linux_identity_registry_add(const hl_linux_pidmap_update *updates, size_t count);
uint64_t hl_linux_identity_registry_commit_word(const hl_linux_identity_registry *registry);
int hl_linux_identity_registry_setsid(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, hl_linux_pidmap *sid, int32_t guest,
                                      int32_t *host_sid);
int hl_linux_identity_registry_setpgid(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, int32_t guest_process,
                                       int32_t host_process, int32_t guest_group, int32_t host_group);
int hl_linux_identity_registry_reap(hl_linux_pidmap *pid, hl_linux_pidmap *pgid, hl_linux_pidmap *sid,
                                    int32_t host_process);
#if defined(HL_NATIVE_TEST_HOOKS)
int hl_c_backend_identity_registry_test(uint32_t scenario, uint32_t iterations);
#endif
int hl_linux_pidmap_add(hl_linux_pidmap *map, int32_t guest, int32_t host);
int32_t hl_linux_pidmap_allocate_guest(hl_linux_pidmap *map);
int32_t hl_linux_pidmap_register_host(hl_linux_pidmap *map, int32_t host);
int hl_linux_pidmap_remove_host(hl_linux_pidmap *map, int32_t host);
void hl_linux_pidmap_activate(hl_linux_pidmap *map);
int hl_linux_pidmap_is_active(const hl_linux_pidmap *map);
int hl_linux_pidmap_host_checked(const hl_linux_pidmap *map, int32_t guest, int32_t *host);
int hl_linux_pidmap_guest_checked(const hl_linux_pidmap *map, int32_t host, int32_t *guest);
int32_t hl_linux_pidmap_host(const hl_linux_pidmap *map, int32_t guest);
int32_t hl_linux_pidmap_guest(const hl_linux_pidmap *map, int32_t host);
uint32_t hl_linux_pidmap_count(const hl_linux_pidmap *map);
size_t hl_linux_pidmap_snapshot(const hl_linux_pidmap *map, hl_linux_pidmap_entry *entries, size_t capacity);
int hl_linux_pidmap_snapshot_checked(const hl_linux_pidmap *map, hl_linux_pidmap_entry *entries, size_t capacity,
                                     size_t *count);

#endif
