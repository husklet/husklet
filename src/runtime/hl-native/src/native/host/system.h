#ifndef HL_HOST_SYSTEM_H
#define HL_HOST_SYSTEM_H

/* Native descriptors kept disjoint from the guest-visible interval. */
#define HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM 4096u
#define HL_HOST_GUEST_DESCRIPTOR_MINIMUM 20480u

void hl_host_private_init(void);

#include <stddef.h>
#include <stdint.h>

typedef struct hl_host_cpu_ticks {
    uint64_t user;
    uint64_t nice;
    uint64_t system;
    uint64_t idle;
} hl_host_cpu_ticks;

typedef struct hl_host_system_info {
    uint64_t boot_time_seconds;
    uint64_t memory_total;
    uint64_t memory_free;
    uint64_t memory_available;
    uint64_t memory_cached;
    uint32_t online_cpus;
    uint32_t reported_cores;
    hl_host_cpu_ticks aggregate;
} hl_host_system_info;

typedef struct hl_host_process_info {
    int64_t parent_pid;
    int64_t process_group;
    int64_t session;
    uint64_t resident_bytes;
    uint64_t virtual_bytes;
    uint64_t user_time_ns;
    uint64_t system_time_ns;
    uint64_t start_time_seconds;
    uint64_t start_time_ns;
    uint32_t threads;
    char state;
    char name[32];
} hl_host_process_info;

enum {
    HL_HOST_FD_OTHER = 0,
    HL_HOST_FD_FILE = 1,
    HL_HOST_FD_PIPE = 2,
    HL_HOST_FD_SOCKET = 3,
};

enum { HL_HOST_PROCESS_FD_ENGINE_PRIVATE = 1u << 0 };

typedef struct hl_host_process_fd {
    int32_t descriptor;
    uint32_t kind;
    uint32_t flags;
    uint32_t reserved;
    uint64_t stable_device;
    uint64_t stable_object;
} hl_host_process_fd;

typedef struct hl_host_process_peer {
    int64_t identity;
} hl_host_process_peer;

/* Snapshot host-wide values and up to core_capacity per-core counters. */
int hl_host_system_read(hl_host_system_info *info, hl_host_cpu_ticks *cores, size_t core_capacity);

/* Snapshot one live native process. Returns zero when the pid is absent or inaccessible. */
int hl_host_process_read(int64_t pid, hl_host_process_info *info);

/* Enumerate descriptor numbers. kind may remain OTHER until fd_read; count includes truncated entries. */
int hl_host_process_fds(int64_t pid, hl_host_process_fd *entries, size_t capacity, size_t *count);
int hl_host_process_fd_private_add(int descriptor);
/* Takes ownership of descriptor on success and returns its relocated engine-private number; leaves the
 * input open and returns a negative errno on failure. */
int hl_host_process_fd_private_adopt(int descriptor);
int hl_host_process_fd_private_floor(void);
void hl_host_process_fd_private_remove(int descriptor);
int hl_host_process_fd_private_is(int64_t pid, uint64_t start_ns, int descriptor);
int hl_host_process_fd_private_current(int descriptor);
int hl_host_process_fd_private_fork_prepare(void);
int hl_host_process_fd_private_fork_complete(int child);
void hl_host_process_fd_private_cleanup(void);

/* Query one open descriptor and, for files, copy its native absolute path without a trailing NUL. */
int hl_host_process_fd_read(int64_t pid, int32_t descriptor, hl_host_process_fd *entry, char *path,
                            size_t path_capacity, size_t *path_size);

/* Enumerate other instances of this executable in the current process session. count includes truncation. */
int hl_host_process_peers(hl_host_process_peer *entries, size_t capacity, size_t *count);

/* Interrupt one enumerated peer so it can reach an engine safepoint. */
int hl_host_process_interrupt(hl_host_process_peer peer);

#endif
